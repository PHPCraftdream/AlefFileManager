// SPDX-License-Identifier: MIT OR Apache-2.0
// Temporary transport spike (docs/TRANSPORT.md); enabled by ALEF_TRANSPORT_SPIKE=1.
use std::io;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use net::fetch::methods::Data;
use serde_json::json;
use servo::protocol_handler::{DoneChannel, HttpStatus, Request, Response, ResponseBody};
use tokio::sync::mpsc::{unbounded_channel, UnboundedSender};
use url::Url;

use super::transport::read_body;
use super::{apply_headers, authorize, MemoryProtocol};

pub(crate) fn enabled() -> bool {
    std::env::var("ALEF_TRANSPORT_SPIKE").is_ok_and(|value| value == "1")
}

pub(crate) struct StreamQuery {
    chunks: u32,
    size: usize,
    delay: Duration,
    label: String,
}

impl StreamQuery {
    pub(crate) fn parse(url: &Url) -> Self {
        let mut query = Self {
            chunks: 10,
            size: 1024,
            delay: Duration::ZERO,
            label: String::new(),
        };
        for (key, value) in url.query_pairs() {
            match key.as_ref() {
                "chunks" => query.chunks = value.parse().unwrap_or(query.chunks),
                "size" => query.size = value.parse::<usize>().unwrap_or(query.size).max(12),
                "delay_ms" => query.delay = Duration::from_millis(value.parse().unwrap_or(0)),
                "label" => query.label = value.into_owned(),
                _ => {}
            }
        }
        query
    }
}

// Guarantees a terminal message: Servo panics if the channel closes without one.
struct Terminal(UnboundedSender<Data>, bool);
impl Drop for Terminal {
    fn drop(&mut self) {
        if !self.1 {
            let _ = self.0.send(Data::Cancelled);
        }
    }
}

fn now_ms() -> f64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0.0, |elapsed| elapsed.as_secs_f64() * 1000.0)
}

/// Installs the streaming body channel for this fetch.
pub(crate) fn install(done_chan: &mut DoneChannel) -> UnboundedSender<Data> {
    let (sender, receiver) = unbounded_channel();
    *done_chan = Some((sender.clone(), receiver));
    sender
}

/// Produces chunks = [sent_ms: f64 LE][index: u32 LE][padding].
pub(crate) fn start_stream(
    query: StreamQuery,
    sender: UnboundedSender<Data>,
    handle: &tokio::runtime::Handle,
) {
    handle.spawn(async move {
        let mut terminal = Terminal(sender, false);
        let started = Instant::now();
        for index in 0..query.chunks {
            if !query.delay.is_zero() {
                tokio::time::sleep(query.delay).await;
            }
            let mut chunk = vec![0u8; query.size];
            chunk[..8].copy_from_slice(&now_ms().to_le_bytes());
            chunk[8..12].copy_from_slice(&index.to_le_bytes());
            if terminal.0.send(Data::Payload(chunk.into())).is_err() {
                eprintln!(
                    "spike: [{}] receiver closed at chunk {index} after {:?}",
                    query.label,
                    started.elapsed()
                );
                return;
            }
        }
        terminal.1 = true;
        let done = terminal.0.send(Data::Done).is_ok();
        eprintln!(
            "spike: [{}] sent {} x {} B in {:?}, done delivered={done}",
            query.label,
            query.chunks,
            query.size,
            started.elapsed()
        );
    });
}

impl MemoryProtocol {
    pub(super) async fn spike(
        &self,
        request: &mut Request,
        stream: Option<tokio::sync::mpsc::UnboundedSender<net::fetch::methods::Data>>,
        mut response: Response,
        url: &Url,
    ) -> Response {
        let result: io::Result<Option<(Vec<u8>, &str)>> = async {
            if request.method == http::Method::OPTIONS {
                return Ok(Some((Vec::new(), "application/json")));
            }
            authorize(&self.token, &request.headers)?;
            match url.path() {
                "/stream" => {
                    let sender = stream.ok_or_else(|| {
                        io::Error::new(io::ErrorKind::InvalidInput, "Stream requires GET")
                    })?;
                    start_stream(StreamQuery::parse(url), sender, &self.handle);
                    Ok(None)
                }
                "/source" => {
                    let size = url
                        .query_pairs()
                        .find(|(key, _)| key == "size")
                        .and_then(|(_, value)| value.parse::<usize>().ok())
                        .unwrap_or(0);
                    Ok(Some((vec![7u8; size], "application/octet-stream")))
                }
                "/echo" | "/report" | "/sink" => {
                    let body = request.body.take().ok_or_else(|| {
                        io::Error::new(io::ErrorKind::InvalidInput, "Missing body")
                    })?;
                    let bytes = read_body(body, 128 * 1024 * 1024).await?;
                    if url.path() == "/sink" {
                        Ok(Some((
                            bytes.len().to_string().into_bytes(),
                            "application/json",
                        )))
                    } else if url.path() == "/report" {
                        eprintln!("spike report: {}", String::from_utf8_lossy(&bytes));
                        Ok(Some((b"null".to_vec(), "application/json")))
                    } else {
                        Ok(Some((bytes, "application/octet-stream")))
                    }
                }
                _ => Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    "Unknown spike route",
                )),
            }
        }
        .await;
        match result {
            Ok(None) => {
                response.status = HttpStatus::new_raw(200, vec![]);
                apply_headers(&mut response, "application/octet-stream", None);
                *response.body.lock() = ResponseBody::Receiving(Vec::new());
            }
            Ok(Some((bytes, content_type))) => {
                response.status = HttpStatus::new_raw(200, vec![]);
                apply_headers(&mut response, content_type, None);
                *response.body.lock() = ResponseBody::Done(bytes);
            }
            Err(error) => {
                eprintln!("spike: request failed: {error}");
                response.status = HttpStatus::new_raw(500, vec![]);
                apply_headers(&mut response, "application/json", None);
                *response.body.lock() = ResponseBody::Done(
                    serde_json::to_vec(&json!({"error": error.to_string()})).expect("JSON string"),
                );
            }
        }
        response
    }
}
