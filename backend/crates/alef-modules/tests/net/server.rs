// SPDX-License-Identifier: MIT OR Apache-2.0
//! A small HTTP server on the loopback for the tests of `http`: it answers by the path, and it
//! remembers what it was sent.
use std::{
    collections::BTreeMap,
    convert::Infallible,
    io,
    net::SocketAddr,
    sync::{Arc, Mutex},
    time::Duration,
};

use bytes::Bytes;
use http_body_util::{channel::Channel, combinators::BoxBody, BodyExt, Empty, Full};
use hyper::{
    body::Incoming, server::conn::http1, service::service_fn, Request, Response, StatusCode,
};
use hyper_util::rt::TokioIo;
use tokio::net::TcpListener;

/// What the server saw of a request.
#[derive(Debug, Clone)]
pub struct Seen {
    pub method: String,
    pub path: String,
    pub headers: BTreeMap<String, String>,
    pub body: Vec<u8>,
}

/// What the server knows: the requests it saw, and the paths of those that were dropped before they
/// were answered (the client closed the connection).
#[derive(Clone, Default)]
struct Log {
    seen: Arc<Mutex<Vec<Seen>>>,
    cut: Arc<Mutex<Vec<String>>>,
}

/// Notes a path in the log of the cut requests unless it is finished first.
struct Unfinished {
    path: &'static str,
    log: Log,
    answered: bool,
}

impl Drop for Unfinished {
    fn drop(&mut self) {
        if !self.answered {
            self.log.cut.lock().unwrap().push(self.path.to_owned());
        }
    }
}

pub struct Server {
    pub address: SocketAddr,
    log: Log,
}

/// The bytes of `/big`: a pattern a test can work out again.
pub fn big_byte(at: usize) -> u8 {
    ((at * 31 + (at >> 8)) & 255) as u8
}

pub const BIG: usize = 8 * 1024 * 1024;

impl Server {
    pub async fn start() -> Self {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("a port");
        let address = listener.local_addr().expect("an address");
        let log = Log::default();
        let shared = log.clone();
        tokio::spawn(async move {
            loop {
                let Ok((stream, _)) = listener.accept().await else {
                    return;
                };
                let log = log.clone();
                tokio::spawn(async move {
                    let service = service_fn(move |request| {
                        let log = log.clone();
                        async move { Ok::<_, Infallible>(answer(request, log).await) }
                    });
                    let _ = http1::Builder::new()
                        .serve_connection(TokioIo::new(stream), service)
                        .await;
                });
            }
        });
        Self {
            address,
            log: shared,
        }
    }

    /// An address that takes connections and closes them at once, without an answer.
    pub async fn hangup() -> SocketAddr {
        let listener = TcpListener::bind("127.0.0.1:0").await.expect("a port");
        let address = listener.local_addr().expect("an address");
        tokio::spawn(async move {
            while let Ok((stream, _)) = listener.accept().await {
                drop(stream);
            }
        });
        address
    }

    /// The pattern of a scope that covers everything of this server.
    pub fn scope(&self) -> String {
        format!("http://127.0.0.1:{}/*", self.address.port())
    }

    pub fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.address.port())
    }

    pub fn requests(&self) -> Vec<Seen> {
        self.log.seen.lock().unwrap().clone()
    }

    /// The paths of requests that were dropped before the server answered them.
    pub fn cut(&self) -> Vec<String> {
        self.log.cut.lock().unwrap().clone()
    }
}

type Body = BoxBody<Bytes, io::Error>;

fn full(bytes: impl Into<Bytes>) -> Body {
    Full::new(bytes.into())
        .map_err(|never| match never {})
        .boxed()
}

fn plain(status: u16, text: &'static str) -> Response<Body> {
    Response::builder()
        .status(status)
        .body(full(text))
        .expect("a response")
}

fn query(request: &Request<Incoming>, name: &str) -> Option<String> {
    request
        .uri()
        .query()?
        .split('&')
        .find_map(|pair| pair.strip_prefix(&format!("{name}=")).map(str::to_owned))
}

async fn answer(request: Request<Incoming>, log: Log) -> Response<Body> {
    let method = request.method().to_string();
    let path = request.uri().path().to_owned();
    let mut headers = BTreeMap::<String, String>::new();
    for (name, value) in request.headers() {
        let value = String::from_utf8_lossy(value.as_bytes()).into_owned();
        headers
            .entry(name.as_str().to_owned())
            .and_modify(|seen| {
                seen.push_str(", ");
                seen.push_str(&value);
            })
            .or_insert(value);
    }
    let target = query(&request, "to");
    let body = match request.into_body().collect().await {
        Ok(collected) => collected.to_bytes().to_vec(),
        Err(_) => Vec::new(),
    };
    log.seen.lock().unwrap().push(Seen {
        method: method.clone(),
        path: path.clone(),
        headers,
        body: body.clone(),
    });
    match path.as_str() {
        "/hello" => Response::builder()
            .header("x-test", "a")
            .header("content-type", "text/plain")
            .body(full("hello"))
            .expect("a response"),
        "/echo" => Response::builder()
            .header("x-method", method)
            .body(full(body))
            .expect("a response"),
        "/sum" => {
            // What arrived, as one number: a body that is wrong anywhere changes it.
            let sum = body.iter().enumerate().fold(0_u64, |sum, (at, byte)| {
                sum.wrapping_add((at as u64 + 1) * u64::from(*byte))
            });
            Response::builder()
                .header("x-length", body.len().to_string())
                .body(full(format!("{sum}")))
                .expect("a response")
        }
        "/nobody" => Response::builder()
            .status(StatusCode::NO_CONTENT)
            .body(Empty::new().map_err(|never| match never {}).boxed())
            .expect("a response"),
        "/slow" => {
            let mut waiting = Unfinished {
                path: "/slow",
                log,
                answered: false,
            };
            tokio::time::sleep(Duration::from_secs(3)).await;
            waiting.answered = true;
            plain(200, "late")
        }
        "/notfound" => plain(404, "no such thing"),
        "/big" => {
            let (mut sender, channel) = Channel::<Bytes, io::Error>::new(2);
            tokio::spawn(async move {
                let mut at = 0;
                while at < BIG {
                    let end = (at + 64 * 1024).min(BIG);
                    let piece: Vec<u8> = (at..end).map(big_byte).collect();
                    if sender.send_data(Bytes::from(piece)).await.is_err() {
                        return;
                    }
                    at = end;
                }
            });
            Response::builder()
                .header("content-length", BIG.to_string())
                .body(channel.boxed())
                .expect("a response")
        }
        "/cut" => {
            // Promises a megabyte, sends a hundred kilobytes and breaks the connection.
            let (mut sender, channel) = Channel::<Bytes, io::Error>::new(2);
            tokio::spawn(async move {
                let _ = sender.send_data(Bytes::from(vec![7_u8; 100 * 1024])).await;
                tokio::time::sleep(Duration::from_millis(100)).await;
                sender.abort(io::Error::other("cut"));
            });
            Response::builder()
                .header("content-length", (1024 * 1024).to_string())
                .body(channel.boxed())
                .expect("a response")
        }
        "/loop" => Response::builder()
            .status(302)
            .header("location", "/loop")
            .body(full(""))
            .expect("a response"),
        other => match other
            .strip_prefix("/redirect/")
            .and_then(|code| code.parse::<u16>().ok())
        {
            Some(code) => Response::builder()
                .status(code)
                .header("location", target.unwrap_or_else(|| "/hello".to_owned()))
                .body(full(""))
                .expect("a response"),
            None => plain(404, "unknown"),
        },
    }
}
