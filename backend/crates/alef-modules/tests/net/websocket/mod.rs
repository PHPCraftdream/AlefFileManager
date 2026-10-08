// SPDX-License-Identifier: MIT OR Apache-2.0
//! `websocket` through the registry, against servers of the protocol on the loopback: the messages each
//! way, the closes, the handshake and what it is held to, the rights, what the user substituted.
use std::{
    collections::BTreeMap,
    net::SocketAddr,
    sync::{Arc, Mutex},
};

use alef_core::{registry::command::Reply, AlefError, ErrorCode};
use async_tungstenite::{
    tokio::accept_hdr_async,
    tungstenite::{
        handshake::server::{Request, Response},
        http::HeaderValue,
        protocol::{frame::coding::CloseCode, CloseFrame},
        Message,
    },
};
use bytes::Bytes;
use futures_util::StreamExt;
use serde_json::{json, Value};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};
use tokio_rustls::TlsAcceptor;

use crate::{common::Fixture, shared::pipe::Pipe};

mod handshake;
mod messages;

/// A message a server got.
#[derive(Debug, Clone, PartialEq)]
pub enum Received {
    Text(String),
    Binary(Vec<u8>),
}

/// What a server knows of one connection to it.
#[derive(Debug, Clone, Default)]
pub struct Visit {
    pub path: String,
    pub headers: BTreeMap<String, String>,
    pub received: Vec<Received>,
    pub close: Option<(u16, String)>,
    /// The connection is over (the server read to its end).
    pub ended: bool,
}

/// A server of the protocol that behaves by the path: `/echo` gives back what it gets; `/bye` says
/// something and closes with 4001; `/nocode` closes without a code; `/abrupt` says something and drops the
/// connection; `/broken` sends a frame that breaks the protocol; `/big` sends 5 MiB first, then echoes. It
/// offers the subprotocol `superchat` to whoever asks for it.
pub struct WsServer {
    pub address: SocketAddr,
    visits: Arc<Mutex<Vec<Visit>>>,
}

pub const BIG: usize = 5 * 1024 * 1024;

pub fn pattern(count: usize) -> Vec<u8> {
    (0..count)
        .map(|at| ((at * 31 + (at >> 8)) & 255) as u8)
        .collect()
}

impl WsServer {
    pub async fn start() -> Self {
        Self::listen(None).await
    }

    /// The same, behind TLS (the certificate of the authority of the tests).
    pub async fn start_secure(acceptor: TlsAcceptor) -> Self {
        Self::listen(Some(acceptor)).await
    }

    async fn listen(acceptor: Option<TlsAcceptor>) -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let visits = Arc::new(Mutex::new(Vec::new()));
        let shared = visits.clone();
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                let visits = shared.clone();
                let acceptor = acceptor.clone();
                tokio::spawn(async move {
                    match acceptor {
                        None => serve(stream, visits).await,
                        Some(acceptor) => {
                            if let Ok(stream) = acceptor.accept(stream).await {
                                serve(stream, visits).await;
                            }
                        }
                    }
                });
            }
        });
        Self { address, visits }
    }

    pub fn url(&self, path: &str) -> String {
        format!("ws://127.0.0.1:{}{path}", self.address.port())
    }

    pub fn secure_url(&self, path: &str) -> String {
        format!("wss://127.0.0.1:{}{path}", self.address.port())
    }

    /// The pattern of a scope that covers everything of this server, in `ws`.
    pub fn scope(&self) -> String {
        format!("ws://127.0.0.1:{}/*", self.address.port())
    }

    pub fn secure_scope(&self) -> String {
        format!("wss://127.0.0.1:{}/*", self.address.port())
    }

    pub fn visits(&self) -> Vec<Visit> {
        self.visits.lock().unwrap().clone()
    }
}

async fn serve<S>(stream: S, visits: Arc<Mutex<Vec<Visit>>>)
where
    S: AsyncRead + AsyncWrite + Unpin + Send + 'static,
{
    let seen = Arc::new(Mutex::new((String::new(), BTreeMap::new())));
    let noting = seen.clone();
    // The error the library asks the callback to be able to return is big; this one never returns it.
    #[allow(clippy::result_large_err)]
    let callback = move |request: &Request, mut response: Response| {
        let mut headers = BTreeMap::new();
        for (name, value) in request.headers() {
            headers.insert(
                name.as_str().to_owned(),
                String::from_utf8_lossy(value.as_bytes()).into_owned(),
            );
        }
        if headers
            .get("sec-websocket-protocol")
            .is_some_and(|offered| offered.split(',').any(|name| name.trim() == "superchat"))
        {
            response.headers_mut().insert(
                "sec-websocket-protocol",
                HeaderValue::from_static("superchat"),
            );
        }
        *noting.lock().unwrap() = (request.uri().path().to_owned(), headers);
        Ok(response)
    };
    let Ok(mut ws) = accept_hdr_async(stream, callback).await else {
        return;
    };
    let (path, headers) = seen.lock().unwrap().clone();
    let index = {
        let mut all = visits.lock().unwrap();
        all.push(Visit {
            path: path.clone(),
            headers,
            ..Visit::default()
        });
        all.len() - 1
    };
    match path.as_str() {
        "/bye" => {
            let _ = ws.send(Message::text("last")).await;
            let farewell = CloseFrame {
                code: CloseCode::from(4001),
                reason: "bye".into(),
            };
            let _ = ws.close(Some(farewell)).await;
            while let Some(Ok(_)) = ws.next().await {}
            return;
        }
        "/nocode" => {
            let _ = ws.close(None).await;
            while let Some(Ok(_)) = ws.next().await {}
            return;
        }
        "/broken" => {
            use tokio::io::AsyncWriteExt;
            let mut stream = ws.into_inner().into_inner();
            // A text frame whose bytes are not UTF-8.
            let _ = stream.write_all(&[0x81, 0x02, 0xff, 0xfe]).await;
            let _ = stream.flush().await;
            return;
        }
        "/abrupt" => {
            let _ = ws.send(Message::text("x")).await;
            drop(ws.into_inner());
            return;
        }
        "/big" => {
            let _ = ws.send(Message::binary(pattern(BIG))).await;
        }
        _ => {}
    }
    while let Some(Ok(message)) = ws.next().await {
        let answer = note(&visits, index, message);
        match answer {
            Answer::Send(message) => {
                let _ = ws.send(message).await;
            }
            Answer::Close => {
                // The answer to the close of the peer goes out on the next read, which ends the connection.
                while let Some(Ok(_)) = ws.next().await {}
                break;
            }
            Answer::Nothing => {}
        }
    }
    visits.lock().unwrap()[index].ended = true;
}

enum Answer {
    Send(Message),
    Close,
    Nothing,
}

/// Notes what the server got, and says what it does about it.
fn note(visits: &Mutex<Vec<Visit>>, index: usize, message: Message) -> Answer {
    let mut all = visits.lock().unwrap();
    let visit = &mut all[index];
    match message {
        Message::Text(text) => {
            visit.received.push(Received::Text(text.to_string()));
            Answer::Send(Message::text(text.to_string()))
        }
        Message::Binary(bytes) => {
            visit.received.push(Received::Binary(bytes.to_vec()));
            Answer::Send(Message::binary(bytes))
        }
        Message::Close(frame) => {
            visit.close = Some(
                frame
                    .map(|frame| (u16::from(frame.code), frame.reason.to_string()))
                    .unwrap_or((1005, String::new())),
            );
            Answer::Close
        }
        _ => Answer::Nothing,
    }
}

/// A server that answers every request with the same bytes and hangs up: no server of the protocol.
pub async fn canned(response: &'static str) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut seen = Vec::new();
                let mut chunk = [0_u8; 1024];
                while !seen.windows(4).any(|window| window == b"\r\n\r\n") {
                    match stream.read(&mut chunk).await {
                        Ok(0) | Err(_) => return,
                        Ok(count) => seen.extend_from_slice(&chunk[..count]),
                    }
                }
                let _ = stream.write_all(response.as_bytes()).await;
                let _ = stream.shutdown().await;
            });
        }
    });
    address
}

/// A server that takes connections and says nothing to any of them.
pub async fn silent() -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    tokio::spawn(async move {
        let mut held: Vec<TcpStream> = Vec::new();
        while let Ok((stream, _)) = listener.accept().await {
            held.push(stream);
        }
    });
    address
}

/// What the page sees on the stream of messages.
#[derive(Debug, PartialEq)]
pub enum Event {
    Text(String),
    Binary(Vec<u8>),
    Close {
        code: u64,
        reason: String,
        clean: bool,
    },
}

/// The next event of the stream, `None` at its end.
pub async fn event(messages: &mut Pipe) -> Option<Event> {
    let header = messages.json().await?;
    match header["type"].as_str().expect("a kind") {
        "close" => Some(Event::Close {
            code: header["code"].as_u64().unwrap(),
            reason: header["reason"].as_str().unwrap().to_owned(),
            clean: header["clean"].as_bool().unwrap(),
        }),
        kind => {
            let length = header["length"].as_u64().unwrap() as usize;
            let bytes = messages.exactly(length).await;
            Some(if kind == "text" {
                Event::Text(String::from_utf8(bytes).expect("text is UTF-8"))
            } else {
                Event::Binary(bytes)
            })
        }
    }
}

/// A connection as the page has it.
pub struct Open {
    pub id: u64,
    pub messages: Pipe,
    pub protocol: String,
    pub url: String,
}

/// Waits (a few seconds at most) until a condition holds.
pub async fn until(mut condition: impl FnMut() -> bool) -> bool {
    for _ in 0..60 {
        if condition() {
            return true;
        }
        tokio::time::sleep(std::time::Duration::from_millis(50)).await;
    }
    condition()
}

pub async fn connect(app: &Fixture, url: &str, extra: Value) -> Result<Open, AlefError> {
    let mut args = json!({ "url": url });
    for (name, value) in extra.as_object().into_iter().flatten() {
        args[name] = value.clone();
    }
    let reply = app.call("websocket.connect", args).await?;
    Ok(Open {
        id: reply["socket"].as_u64().unwrap(),
        messages: Pipe::open(app, reply["messages"].as_u64().unwrap()),
        protocol: reply["protocol"].as_str().unwrap().to_owned(),
        url: reply["url"].as_str().unwrap().to_owned(),
    })
}

pub async fn send(app: &Fixture, id: u64, text: bool, data: &[u8]) -> Result<Value, AlefError> {
    match app
        .call_reply(
            "websocket.send",
            json!({ "socket": id, "text": text }),
            Some(Bytes::copy_from_slice(data)),
        )
        .await?
    {
        Reply::Json(value) => Ok(value),
        other => panic!("expected JSON, got {other:?}"),
    }
}

pub async fn close(
    app: &Fixture,
    id: u64,
    code: Option<u16>,
    reason: &str,
) -> Result<Value, AlefError> {
    let mut args = json!({ "socket": id });
    if let Some(code) = code {
        args["code"] = json!(code);
        args["reason"] = json!(reason);
    }
    app.call("websocket.close", args).await
}

pub fn code<T>(result: Result<T, AlefError>) -> ErrorCode {
    match result {
        Ok(_) => panic!("an error was expected"),
        Err(error) => error.code,
    }
}
