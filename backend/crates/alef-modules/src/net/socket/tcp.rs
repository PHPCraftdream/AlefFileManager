// SPDX-License-Identifier: MIT OR Apache-2.0
//! TCP: a connection (plain or TLS) is two streams, and a server is a stream of connections. The bytes
//! move in tasks that wait for credit, so a slow page holds the other end back and nothing piles up.
use std::{io, net::SocketAddr, sync::Arc, time::Duration};

use alef_core::{
    registry::dispatch::Registry,
    security::{consent::Decision, permissions::Permission},
    session::{
        session::Session,
        streams::{IncomingReader, StreamWriter},
    },
    AlefError, ErrorCode,
};
use bytes::Bytes;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    sync::Notify,
};

use super::{address, dead, network, target, tls, Socket, LOOPBACK};
use crate::json;

/// How many bytes one read of a socket takes at the most.
const READ_CHUNK: usize = 32 * 1024;
/// How long an accept that fails waits before it tries again (a process out of descriptors, say).
const ACCEPT_PAUSE: Duration = Duration::from_millis(50);

/// What the bytes of a connection go through, whether TLS wraps them or not.
trait Io: AsyncRead + AsyncWrite + Send + Unpin + 'static {}
impl<T: AsyncRead + AsyncWrite + Send + Unpin + 'static> Io for T {}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ConnectArgs {
    host: String,
    port: u16,
    #[serde(default)]
    tls: Option<TlsArg>,
    #[serde(default)]
    timeout_ms: Option<u64>,
}

/// `tls: true`, or the options of TLS.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
enum TlsArg {
    Switch(bool),
    Options(TlsOptions),
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct TlsOptions {
    #[serde(default)]
    server_name: Option<String>,
    #[serde(default)]
    ca: Option<String>,
}

fn tls_options(arg: Option<TlsArg>) -> Option<TlsOptions> {
    match arg {
        None | Some(TlsArg::Switch(false)) => None,
        Some(TlsArg::Switch(true)) => Some(TlsOptions::default()),
        Some(TlsArg::Options(options)) => Some(options),
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ListenArgs {
    #[serde(default)]
    host: Option<String>,
    #[serde(default)]
    port: u16,
}

/// Sends what comes from the socket to the page, as the page takes it; the end of the socket is the
/// end of the stream.
async fn read_pump<R: AsyncRead + Unpin>(mut half: R, writer: StreamWriter) {
    let mut buffer = vec![0_u8; READ_CHUNK];
    loop {
        match half.read(&mut buffer).await {
            Ok(0) => {
                writer.end();
                return;
            }
            Ok(count) => {
                if writer
                    .send_binary(Bytes::copy_from_slice(&buffer[..count]))
                    .await
                    .is_err()
                {
                    return;
                }
            }
            // A peer that closes TLS without saying so is a peer that is done.
            Err(error) if error.kind() == io::ErrorKind::UnexpectedEof => {
                writer.end();
                return;
            }
            Err(error) => {
                writer.error(network(format!("the connection broke: {error}")));
                return;
            }
        }
    }
}

/// Writes what the page sends to the socket; the end of the stream is the end of the socket's output.
/// When the socket is closed (`finish`) what the page sent already still goes out, then the output ends.
async fn write_pump<W: AsyncWrite + Unpin>(
    mut half: W,
    mut reader: IncomingReader,
    finish: Arc<Notify>,
) {
    loop {
        tokio::select! {
            piece = reader.recv() => match piece {
                Some(Ok(bytes)) => {
                    if half.write_all(&bytes).await.is_err() {
                        return;
                    }
                }
                _ => break,
            },
            _ = finish.notified() => {
                while let Ok(Some(Ok(bytes))) =
                    tokio::time::timeout(Duration::ZERO, tokio::task::unconstrained(reader.recv())).await
                {
                    if half.write_all(&bytes).await.is_err() {
                        return;
                    }
                }
                break;
            }
        }
    }
    let _ = half.shutdown().await;
}

/// Makes a resource and two streams of a connection and starts the tasks that move its bytes.
fn attach(
    session: &Arc<Session>,
    io: Box<dyn Io>,
    local: SocketAddr,
    remote: SocketAddr,
) -> Result<Value, AlefError> {
    let socket = Socket::default();
    let finish = socket.finish.clone();
    let id = session.resources().insert(Box::new(socket))?;
    let (reading, writing) = tokio::io::split(io);
    let (writer, read) = session.streams().open_outgoing();
    let (reader, write) = session.streams().open_incoming_reader();
    let reading = tokio::spawn(read_pump(reading, writer));
    let writing = tokio::spawn(write_pump(writing, reader, finish));
    session.resources().with_as::<Socket, _>(id, |socket| {
        socket.adopt([reading]);
        socket.adopt_writing(writing);
    })?;
    Ok(json!({
        "socket": id.0,
        "read": read.0,
        "write": write.0,
        "localAddress": address(local),
        "remoteAddress": address(remote),
    }))
}

async fn open(
    host: &str,
    port: u16,
    tls: Option<TlsOptions>,
) -> Result<(Box<dyn Io>, SocketAddr, SocketAddr), AlefError> {
    let stream = TcpStream::connect((host, port))
        .await
        .map_err(|error| network(format!("cannot connect to the server: {error}")))?;
    let _ = stream.set_nodelay(true);
    let (local, remote) = (stream.local_addr()?, stream.peer_addr()?);
    let io: Box<dyn Io> = match tls {
        None => Box::new(stream),
        Some(options) => {
            let connector = tls::connector(options.ca.as_deref())?;
            let name = tls::server_name(host, options.server_name.as_deref())?;
            let stream = connector
                .connect(name, stream)
                .await
                .map_err(|error| network(format!("the TLS handshake failed: {error}")))?;
            Box::new(stream)
        }
    };
    Ok((io, local, remote))
}

/// Takes the connections of a listener and hands each to the page as a frame of the stream `writer`.
async fn accept_loop(listener: TcpListener, writer: StreamWriter, session: Arc<Session>) {
    loop {
        let (stream, remote) = match listener.accept().await {
            Ok(accepted) => accepted,
            Err(_) => {
                tokio::time::sleep(ACCEPT_PAUSE).await;
                continue;
            }
        };
        let _ = stream.set_nodelay(true);
        let Ok(local) = stream.local_addr() else {
            continue;
        };
        match attach(&session, Box::new(stream), local, remote) {
            Ok(connection) => {
                if writer.send_json(connection).await.is_err() {
                    return;
                }
            }
            Err(error) if error.code == ErrorCode::Closed => return,
            // The session has no room for another connection: this one is refused.
            Err(_) => {}
        }
    }
}

pub(super) fn register(registry: &mut Registry) -> Result<(), AlefError> {
    registry
        .command::<ConnectArgs>("socket.connect")?
        .permission(Permission::NetSocket, |args| {
            Some(target("tcp", &args.host, args.port))
        })
        .substitutes()
        .handler(|ctx, args| async move {
            let ConnectArgs {
                host,
                port,
                tls,
                timeout_ms,
            } = args;
            let limit = timeout_ms.map(Duration::from_millis);
            if ctx.decision() == Decision::Substitute {
                return Err(dead(limit).await);
            }
            let opening = open(&host, port, tls_options(tls));
            let (io, local, remote) = match limit {
                Some(limit) => tokio::time::timeout(limit, opening).await.map_err(|_| {
                    AlefError::new(ErrorCode::Timeout, "the server did not answer in time")
                })??,
                None => opening.await?,
            };
            json(&attach(&ctx.session, io, local, remote)?)
        })?;

    registry
        .command::<ListenArgs>("socket.listen")?
        .permission(Permission::NetSocket, |args| {
            Some(target(
                "listen",
                args.host.as_deref().unwrap_or(LOOPBACK),
                args.port,
            ))
        })
        .substitutes()
        .handler(|ctx, args| async move {
            let host = args.host.as_deref().unwrap_or(LOOPBACK);
            let (writer, accept) = ctx.streams().open_outgoing();
            if ctx.decision() == Decision::Substitute {
                // Nobody comes to a port that was never taken.
                let standing = Socket {
                    _held: vec![writer],
                    ..Socket::default()
                };
                let id = ctx.resources().insert(Box::new(standing))?;
                let local = json!({ "host": host, "port": args.port });
                return json(&json!({ "server": id.0, "accept": accept.0, "localAddress": local }));
            }
            let listener = TcpListener::bind((host, args.port)).await?;
            let local = listener.local_addr()?;
            let id = ctx.resources().insert(Box::<Socket>::default())?;
            let task = tokio::spawn(accept_loop(listener, writer, ctx.session.clone()));
            ctx.resources()
                .with_as::<Socket, _>(id, |socket| socket.adopt([task]))?;
            json(&json!({ "server": id.0, "accept": accept.0, "localAddress": address(local) }))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tls_is_off_unless_it_is_asked_for() {
        assert!(tls_options(None).is_none());
        assert!(tls_options(Some(TlsArg::Switch(false))).is_none());
        let on = tls_options(Some(TlsArg::Switch(true))).unwrap();
        assert!(on.server_name.is_none() && on.ca.is_none());
        let given = tls_options(Some(TlsArg::Options(TlsOptions {
            server_name: Some("a".into()),
            ca: Some("b".into()),
        })))
        .unwrap();
        assert_eq!(given.server_name.as_deref(), Some("a"));
        assert_eq!(given.ca.as_deref(), Some("b"));
    }

    #[test]
    fn the_arguments_of_tls_are_a_switch_or_options_and_nothing_else() {
        let parse = |text: &str| serde_json::from_str::<ConnectArgs>(text);
        let base = |tls: &str| format!(r#"{{ "host": "h", "port": 1, "tls": {tls} }}"#);
        assert!(parse(&base("true")).is_ok());
        assert!(parse(&base("false")).is_ok());
        assert!(parse(&base(r#"{ "serverName": "a", "ca": "b" }"#)).is_ok());
        assert!(parse(&base(r#"{ "verify": false }"#)).is_err());
        assert!(parse(&base(r#""yes""#)).is_err());
        assert!(parse(r#"{ "host": "h", "port": 70000 }"#).is_err());
        assert!(parse(r#"{ "host": "h" }"#).is_err());
    }
}
