// SPDX-License-Identifier: MIT OR Apache-2.0
//! `websocket`: a client of the protocol (RFC 6455) over TCP, with TLS for `wss`. The address is held
//! against `permissions.net.http` (the schemes `ws` and `wss` are in its patterns), and a redirect of
//! the handshake is no answer. Every message that arrives comes through one stream with credit as a frame
//! of JSON that names its kind and its length, and the bytes after it, so a big message never lies whole
//! in the memory of the runtime on its way; a message that goes comes with `websocket.send`, in the body
//! of the call. The right the user substituted is a dead network: the handshake hangs until its timeout.
use std::{sync::Arc, time::Duration};

use alef_core::{
    ids::{ResourceId, StreamId},
    registry::{command::Reply, context::CallContext, dispatch::Registry},
    security::{consent::Decision, permissions::Permission},
    session::{resources::Resource, streams::StreamWriter},
    AlefError, ErrorCode,
};
use async_tungstenite::{
    tokio::{client_async_with_config, TokioAdapter},
    tungstenite::{
        error::ProtocolError,
        protocol::{frame::coding::CloseCode, CloseFrame, Role, WebSocketConfig},
        ClientRequestBuilder, Error as WsError, Message,
    },
    WebSocketReceiver, WebSocketSender, WebSocketStream,
};
use bytes::Bytes;
use futures_util::StreamExt;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::{sync::Mutex, task::JoinHandle};
use url::Url;

use super::{
    headers,
    socket::{dead, open, Io, TlsOptions},
};
use crate::json;

type Wire = TokioAdapter<Box<dyn Io>>;
type Ws = WebSocketStream<Wire>;
type Sink = WebSocketSender<Wire>;

/// The biggest message the runtime takes from a server.
const MAX_INCOMING: usize = 16 * 1024 * 1024;
/// How long a close handshake has, from our frame to the answer of the peer.
const CLOSE_GRACE: Duration = Duration::from_secs(2);
/// The longest reason of a close (the frame of a control carries 125 bytes, two of them are the code).
const MAX_REASON: usize = 123;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ConnectArgs {
    url: String,
    #[serde(default)]
    protocols: Vec<String>,
    #[serde(default)]
    headers: Vec<(String, String)>,
    #[serde(default)]
    ca: Option<String>,
    #[serde(default)]
    timeout_ms: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SendArgs {
    socket: u64,
    #[serde(default)]
    text: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CloseArgs {
    socket: u64,
    #[serde(default)]
    code: Option<u16>,
    #[serde(default)]
    reason: Option<String>,
}

fn invalid(message: &str) -> AlefError {
    AlefError::new(ErrorCode::InvalidArgument, message)
}

fn network(message: impl Into<String>) -> AlefError {
    AlefError::new(ErrorCode::Network, message.into())
}

/// A connection: the half that writes, and the task that reads.
struct Link {
    sink: Arc<Mutex<Sink>>,
    reading: std::sync::Mutex<Option<JoinHandle<()>>>,
}

impl Resource for Link {
    fn close(self: Box<Self>) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
        Box::pin(async move {
            let reading = self
                .reading
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take();
            if let Some(task) = reading {
                task.abort();
                let _ = task.await;
            }
        })
    }
}

/// The codes a page may close with, as in a browser: 1000, or the ones of applications.
fn close_code_allowed(code: u16) -> bool {
    code == 1000 || (3000..=4999).contains(&code)
}

/// A subprotocol is a token (RFC 7230): no separators, no spaces.
pub(in crate::net) fn protocol_is_token(protocol: &str) -> bool {
    !protocol.is_empty()
        && protocol
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || b"!#$%&'*+-.^_`|~".contains(&byte))
}

/// The request of the handshake: the address, the headers of the page, the subprotocols it offers.
fn request(args: &ConnectArgs) -> Result<(ClientRequestBuilder, String, u16, bool), AlefError> {
    let url = Url::parse(&args.url).map_err(|_| invalid("the address is not a URL"))?;
    let secure = match url.scheme() {
        "ws" => false,
        "wss" => true,
        _ => return Err(invalid("an address is ws:// or wss://")),
    };
    if !url.username().is_empty() || url.password().is_some() {
        return Err(invalid(
            "an address has no user name or password: send an Authorization header",
        ));
    }
    let host = url
        .host_str()
        .ok_or_else(|| invalid("an address has a host"))?
        .trim_start_matches('[')
        .trim_end_matches(']')
        .to_owned();
    let port = url
        .port_or_known_default()
        .unwrap_or(if secure { 443 } else { 80 });
    if args.ca.is_some() && !secure {
        return Err(invalid("ca is for wss:// addresses"));
    }
    let map = headers::parse(&args.headers, &["proxy-", "sec-websocket-"])?;
    let mut seen = std::collections::BTreeSet::new();
    for protocol in &args.protocols {
        if !protocol_is_token(protocol) || !seen.insert(protocol.as_str()) {
            return Err(invalid("a subprotocol is a token, and is offered once"));
        }
    }
    let uri = url
        .as_str()
        .parse()
        .map_err(|_| invalid("the address is not a URI"))?;
    let mut builder = ClientRequestBuilder::new(uri);
    let mut agent = false;
    for name in map.keys() {
        agent |= name == hyper::header::USER_AGENT;
        let mut values = Vec::new();
        for value in map.get_all(name) {
            values.push(
                value
                    .to_str()
                    .map_err(|_| invalid("a header has a value that is not text"))?,
            );
        }
        // The library keeps one value of a name: the ones the page gave are joined as HTTP has it.
        let separator = if name == hyper::header::COOKIE {
            "; "
        } else {
            ", "
        };
        builder = builder.with_header(name.as_str(), values.join(separator));
    }
    if !agent {
        builder = builder.with_header("user-agent", concat!("Alef/", env!("CARGO_PKG_VERSION")));
    }
    for protocol in &args.protocols {
        builder = builder.with_sub_protocol(protocol.as_str());
    }
    Ok((builder, host, port, secure))
}

/// Sends a message to the page: its kind and length, then its bytes.
async fn deliver(writer: &StreamWriter, kind: &str, data: &[u8]) -> Result<(), AlefError> {
    writer
        .send_json(json!({ "type": kind, "length": data.len() }))
        .await?;
    writer.send_binary(Bytes::copy_from_slice(data)).await?;
    Ok(())
}

async fn announce_close(writer: &StreamWriter, code: u16, reason: &str, clean: bool) {
    let event = json!({ "type": "close", "code": code, "reason": reason, "clean": clean });
    if writer.send_json(event).await.is_ok() {
        // The stream ends after this; the page reads the close first.
    }
}

/// Hands the messages of the peer to the page until the connection ends.
async fn read_pump(mut stream: WebSocketReceiver<Wire>, writer: StreamWriter) {
    loop {
        match stream.next().await {
            Some(Ok(Message::Text(text))) => {
                if deliver(&writer, "text", text.as_bytes()).await.is_err() {
                    return;
                }
            }
            Some(Ok(Message::Binary(bytes))) => {
                if deliver(&writer, "binary", &bytes).await.is_err() {
                    return;
                }
            }
            Some(Ok(Message::Close(frame))) => {
                let (code, reason) = match frame {
                    Some(frame) => (u16::from(frame.code), frame.reason.to_string()),
                    None => (1005, String::new()),
                };
                announce_close(&writer, code, &reason, true).await;
                writer.end();
                // Our answer to the close of the peer goes out when the connection is read once more,
                // and the peer ends the connection after it.
                let _ = tokio::time::timeout(CLOSE_GRACE, async {
                    while let Some(Ok(_)) = stream.next().await {}
                })
                .await;
                return;
            }
            Some(Ok(_)) => {}
            Some(Err(WsError::ConnectionClosed | WsError::AlreadyClosed))
            | Some(Err(WsError::Protocol(ProtocolError::ResetWithoutClosingHandshake)))
            | None => {
                announce_close(&writer, 1006, "", false).await;
                writer.end();
                return;
            }
            Some(Err(error)) => {
                writer.error(network(format!("the connection broke: {error}")));
                return;
            }
        }
    }
}

fn config() -> WebSocketConfig {
    let mut config = WebSocketConfig::default();
    config.max_message_size = Some(MAX_INCOMING);
    config.max_frame_size = Some(MAX_INCOMING);
    config
}

/// The connection of a client that a server of HTTP took over: the handshake is done, the bytes that follow
/// are frames, and this end is the server's.
pub(in crate::net) async fn server_stream(io: impl Io) -> Ws {
    let wire: Box<dyn Io> = Box::new(io);
    WebSocketStream::from_raw_socket(TokioAdapter::new(wire), Role::Server, Some(config())).await
}

/// Gives a connection to the page: a resource for it, and the stream of the messages that arrive.
pub(in crate::net) fn adopt(
    ctx: &CallContext,
    stream: Ws,
) -> Result<(ResourceId, StreamId), AlefError> {
    let (sink, reading) = stream.split();
    let sink = Arc::new(Mutex::new(sink));
    let link = Link {
        sink: sink.clone(),
        reading: std::sync::Mutex::new(None),
    };
    let id = ctx.resources().insert(Box::new(link))?;
    let (writer, messages) = ctx.streams().open_outgoing();
    let task = tokio::spawn(read_pump(reading, writer));
    ctx.resources().with_as::<Link, _>(id, |link| {
        *link.reading.lock().unwrap_or_else(|e| e.into_inner()) = Some(task);
    })?;
    Ok((id, messages))
}

async fn handshake(args: ConnectArgs) -> Result<(Ws, Option<String>), AlefError> {
    let (builder, host, port, secure) = request(&args)?;
    let tls = secure.then(|| TlsOptions::trusting(args.ca));
    let (io, _, _) = open(&host, port, tls).await?;
    let (stream, response) = client_async_with_config(builder, io, Some(config()))
        .await
        .map_err(|error| match error {
            WsError::Http(response) => network(format!(
                "the server did not upgrade the connection: it answered {}",
                response.status().as_u16()
            )),
            other => network(format!("the handshake failed: {other}")),
        })?;
    let protocol = response
        .headers()
        .get("sec-websocket-protocol")
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned);
    Ok((stream, protocol))
}

pub(crate) fn register(registry: &mut Registry) -> Result<(), AlefError> {
    registry
        .command::<ConnectArgs>("websocket.connect")?
        .permission(Permission::NetHttp, |args| Some(args.url.clone()))
        .substitutes()
        .handler(|ctx, args| async move {
            let limit = args.timeout_ms.map(Duration::from_millis);
            if ctx.decision() == Decision::Substitute {
                return Err(dead(limit).await);
            }
            let url = args.url.clone();
            let connecting = handshake(args);
            let (stream, protocol) = match limit {
                Some(limit) => tokio::time::timeout(limit, connecting)
                    .await
                    .map_err(|_| {
                        AlefError::new(ErrorCode::Timeout, "the server did not answer in time")
                    })??,
                None => connecting.await?,
            };
            let (id, messages) = adopt(&ctx, stream)?;
            json(&json!({
                "socket": id.0,
                "messages": messages.0,
                "protocol": protocol.unwrap_or_default(),
                "url": url,
            }))
        })?;

    registry
        .command::<SendArgs>("websocket.send")?
        .handler(|ctx, args| async move {
            let sink = ctx
                .resources()
                .with_as::<Link, _>(ResourceId(args.socket), |link| link.sink.clone())?;
            let body = ctx.body().cloned().unwrap_or_default();
            let message = if args.text {
                let text = String::from_utf8(body.to_vec())
                    .map_err(|_| invalid("a text message is UTF-8"))?;
                Message::text(text)
            } else {
                Message::binary(body)
            };
            sink.lock()
                .await
                .send(message)
                .await
                .map_err(|error| network(format!("the message was not sent: {error}")))?;
            Ok(Reply::Json(Value::Null))
        })?;

    registry
        .command::<CloseArgs>("websocket.close")?
        .handler(|ctx, args| async move {
            let id = ResourceId(args.socket);
            let code = args.code.unwrap_or(1000);
            let reason = args.reason.unwrap_or_default();
            if !close_code_allowed(code) {
                return Err(invalid("a close code is 1000, or 3000 to 4999"));
            }
            if reason.len() > MAX_REASON {
                return Err(invalid("a reason of a close is at most 123 bytes"));
            }
            let (sink, reading) = ctx.resources().with_as::<Link, _>(id, |link| {
                (
                    link.sink.clone(),
                    link.reading
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .take(),
                )
            })?;
            let frame = CloseFrame {
                code: CloseCode::from(code),
                reason: reason.into(),
            };
            {
                let mut sink = sink.lock().await;
                let _ = tokio::time::timeout(CLOSE_GRACE, sink.close(Some(frame))).await;
            }
            // The peer answers with its own close, which the task that reads hands to the page.
            if let Some(mut reading) = reading {
                if tokio::time::timeout(CLOSE_GRACE, &mut reading)
                    .await
                    .is_err()
                {
                    reading.abort();
                }
            }
            ctx.resources().take(id)?.close().await;
            Ok(Reply::Json(Value::Null))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn args(url: &str) -> ConnectArgs {
        ConnectArgs {
            url: url.to_owned(),
            protocols: Vec::new(),
            headers: Vec::new(),
            ca: None,
            timeout_ms: None,
        }
    }

    #[test]
    fn a_page_closes_with_1000_or_a_code_of_applications() {
        for code in [1000, 3000, 3999, 4000, 4999] {
            assert!(close_code_allowed(code), "{code}");
        }
        for code in [0, 999, 1001, 1005, 1006, 1015, 2999, 5000, u16::MAX] {
            assert!(!close_code_allowed(code), "{code}");
        }
    }

    #[test]
    fn a_subprotocol_is_a_token() {
        for good in ["chat", "v1.json", "a-b_c", "soap+xml", "X"] {
            assert!(protocol_is_token(good), "{good}");
        }
        for bad in ["", "a b", "a,b", "a;b", "a\"b", "a/b", "ü", "a\nb", "(a)"] {
            assert!(!protocol_is_token(bad), "{bad:?}");
        }
    }

    #[test]
    fn an_address_is_ws_or_wss_with_a_host_and_no_user_name_or_password() {
        for bad in [
            "http://h.test/",
            "https://h.test/",
            "ftp://h.test/",
            "not a url",
            "ws://user:pw@h.test/",
            "ws://user@h.test/",
            "ws://:pw@h.test/",
        ] {
            assert!(request(&args(bad)).is_err(), "{bad}");
        }
        let (_, host, port, secure) = request(&args("ws://h.test/chat?x=1")).unwrap();
        assert_eq!((host.as_str(), port, secure), ("h.test", 80, false));
        let (_, host, port, secure) = request(&args("wss://h.test:8443/")).unwrap();
        assert_eq!((host.as_str(), port, secure), ("h.test", 8443, true));
        let (_, host, port, _) = request(&args("ws://[::1]:9/")).unwrap();
        assert_eq!(
            (host.as_str(), port),
            ("::1", 9),
            "no brackets for a connection"
        );
    }

    #[test]
    fn the_headers_the_subprotocols_and_the_authority_are_held_to_their_rules() {
        let mut with = args("ws://h.test/");
        with.headers = vec![("Sec-WebSocket-Key".into(), "x".into())];
        assert!(request(&with).is_err(), "the key is the client's");
        with.headers = vec![
            ("Origin".into(), "https://app.test".into()),
            ("X-One".into(), "1".into()),
        ];
        assert!(request(&with).is_ok());
        with.headers = vec![("Host".into(), "other".into())];
        assert!(request(&with).is_err());

        let mut offered = args("ws://h.test/");
        offered.protocols = vec!["chat".into(), "chat".into()];
        assert!(request(&offered).is_err(), "once");
        offered.protocols = vec!["a b".into()];
        assert!(request(&offered).is_err());
        offered.protocols = vec!["chat".into(), "superchat".into()];
        assert!(request(&offered).is_ok());

        let mut pinned = args("ws://h.test/");
        pinned.ca = Some("PEM".into());
        assert!(request(&pinned).is_err(), "an authority is for wss");
        let mut secure = args("wss://h.test/");
        secure.ca = Some("PEM".into());
        assert!(request(&secure).is_ok());
    }
}
