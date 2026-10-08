// SPDX-License-Identifier: MIT OR Apache-2.0
//! `socket` through the registry, against servers and peers on the loopback: what its streams carry,
//! the rights each command needs, what the user substituted.
use std::{
    net::SocketAddr,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};

use alef_core::{ids::StreamId, session::streams::IncomingWriter, AlefError, ErrorCode};
use bytes::Bytes;
use serde_json::{json, Value};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

use crate::{common::Fixture, shared::pipe::Pipe};

mod tcp;
mod tls;
mod udp;

const MANIFEST: &str = include_str!("../../fixtures/app.ktav");

/// An application that may use the given socket scopes.
fn manifest(sockets: &[&str]) -> String {
    let list = format!("[ {} ]", sockets.join(", "));
    let text = MANIFEST
        .replace('\r', "")
        .replace("        socket: []", &format!("        socket: {list}"));
    assert!(text.contains("socket: [ ") || sockets.is_empty());
    text
}

/// The same, and the given scopes of `net.http` too.
fn manifest_with_http(sockets: &[&str], http: &[String]) -> String {
    manifest(sockets).replace(
        "        http: []",
        &format!("        http: [ {} ]", http.join(", ")),
    )
}

async fn app(sockets: &[&str]) -> Fixture {
    Fixture::new(Some(&manifest(sockets)), &[]).await
}

fn code<T>(result: Result<T, AlefError>) -> ErrorCode {
    match result {
        Ok(_) => panic!("an error was expected"),
        Err(error) => error.code,
    }
}

/// A connection as the page has it: the streams of what comes and of what goes, and the resource.
struct Conn {
    id: u64,
    input: Pipe,
    output: Option<IncomingWriter>,
    local: Value,
    remote: Value,
}

impl Conn {
    fn of(app: &Fixture, reply: &Value) -> Self {
        let output = app
            .session()
            .streams()
            .incoming_writer(StreamId(reply["write"].as_u64().unwrap()))
            .expect("an incoming stream");
        Self {
            id: reply["socket"].as_u64().unwrap(),
            input: Pipe::open(app, reply["read"].as_u64().unwrap()),
            output: Some(output),
            local: reply["localAddress"].clone(),
            remote: reply["remoteAddress"].clone(),
        }
    }

    async fn send(&self, bytes: &[u8]) {
        self.output
            .as_ref()
            .expect("the output is not ended")
            .write(Bytes::copy_from_slice(bytes))
            .await
            .unwrap();
    }

    /// The page has nothing more to send.
    fn finish(&mut self) {
        self.output.take().expect("the output is not ended").end();
    }
}

async fn connect(app: &Fixture, host: &str, port: u16, extra: Value) -> Result<Conn, AlefError> {
    let mut args = json!({ "host": host, "port": port });
    for (name, value) in extra.as_object().into_iter().flatten() {
        args[name] = value.clone();
    }
    let reply = app.call("socket.connect", args).await?;
    Ok(Conn::of(app, &reply))
}

/// A TCP server that gives back every byte it gets and counts the connections it took.
async fn echo() -> (SocketAddr, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let taken = Arc::new(AtomicUsize::new(0));
    let count = taken.clone();
    tokio::spawn(async move {
        while let Ok((mut stream, _)) = listener.accept().await {
            count.fetch_add(1, Ordering::SeqCst);
            tokio::spawn(async move {
                let (mut from, mut to) = stream.split();
                let _ = tokio::io::copy(&mut from, &mut to).await;
                let _ = to.shutdown().await;
            });
        }
    });
    (address, taken)
}

/// A port that nothing listens on.
async fn free_port() -> u16 {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    listener.local_addr().unwrap().port()
}

fn pattern(count: usize) -> Vec<u8> {
    (0..count)
        .map(|at| ((at * 31 + (at >> 8)) & 255) as u8)
        .collect()
}
