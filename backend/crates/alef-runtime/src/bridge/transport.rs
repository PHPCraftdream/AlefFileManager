// SPDX-License-Identifier: MIT OR Apache-2.0
//! Servo glue for `native://call/<command>` and `native://stream/<id>`: translates the fetch into a
//! [`TransportRequest`], and streams the [`TransportResponse`] back through the fetch body channel.
use std::io;

use alef_core::protocol::{
    credit::{chunk, DEFAULT_CHUNK_SIZE},
    transport::{Method, ResponseBody, TransportRequest, TransportResponse},
};
use bytes::Bytes;
use futures_util::StreamExt;
use http::{HeaderName, HeaderValue};
use ipc_channel::ipc;
use net::fetch::methods::Data;
use net_traits::request::{BodyChunkRequest, BodyChunkResponse, RequestBody};
use servo::protocol_handler::{
    HttpStatus, Request, ResourceFetchTiming, Response, ResponseBody as ServoBody,
};
use servo_base::id::PipelineId;
use tokio::{sync::mpsc::UnboundedSender, task::JoinHandle};
use url::Url;

use super::MemoryProtocol;

/// `native://call/<command>` → `call/<command>`, `native://stream/<id>` → `stream/<id>`.
pub(super) fn transport_path(url: &Url) -> Option<String> {
    let kind = url.host_str()?;
    matches!(kind, "call" | "stream").then(|| format!("{kind}{}", url.path()))
}

/// Servo makes a new pipeline for every document load (navigation and reload alike) and numbers
/// them in creation order, so the (namespace, index) pair is the document's ordered identity.
fn document_of(pipeline: PipelineId) -> u64 {
    (u64::from(pipeline.namespace_id.0) << 32) | u64::from(pipeline.index.0.get())
}

fn method_of(method: &http::Method) -> Option<Method> {
    match *method {
        http::Method::GET => Some(Method::Get),
        http::Method::POST => Some(Method::Post),
        http::Method::OPTIONS => Some(Method::Options),
        _ => None,
    }
}

/// Header values that are not valid UTF-8 are dropped, so a mangled `authorization` is a denial.
fn collect_headers(headers: &http::HeaderMap) -> Vec<(String, String)> {
    headers
        .iter()
        .filter_map(|(name, value)| {
            Some((name.as_str().to_owned(), value.to_str().ok()?.to_owned()))
        })
        .collect()
}

/// Guarantees a terminal message on the fetch body channel: Servo panics when the channel closes
/// without `Done`/`Cancelled`.
pub(super) struct Terminal {
    sender: UnboundedSender<Data>,
    finished: bool,
}

impl Terminal {
    pub(super) fn new(sender: UnboundedSender<Data>) -> Self {
        Self {
            sender,
            finished: false,
        }
    }

    /// Sends a chunk; `false` means the fetch is gone and the producer should stop.
    pub(super) fn send(&mut self, chunk: Bytes) -> bool {
        self.sender.send(Data::Payload(chunk)).is_ok()
    }

    /// Normal completion.
    pub(super) fn done(mut self) {
        self.finished = true;
        let _ = self.sender.send(Data::Done);
    }
}

impl Drop for Terminal {
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.sender.send(Data::Cancelled);
        }
    }
}

/// Moves a response body into the fetch channel in chunks of at most 256 KiB, then terminates it.
pub(super) async fn pump(mut terminal: Terminal, body: ResponseBody) {
    match body {
        ResponseBody::Empty => {}
        ResponseBody::Bytes(bytes) => {
            for piece in chunk(bytes, DEFAULT_CHUNK_SIZE) {
                if !terminal.send(piece) {
                    return;
                }
            }
        }
        ResponseBody::Frames(mut frames) => {
            while let Some(piece) = frames.next_chunk().await {
                if !terminal.send(piece) {
                    return;
                }
            }
        }
    }
    terminal.done();
}

fn failure(status: u16, code: &str, message: &str) -> TransportResponse {
    let body = serde_json::json!({ "code": code, "message": message }).to_string();
    TransportResponse {
        status,
        headers: vec![("content-type".into(), "application/json".into())],
        body: ResponseBody::Bytes(body.into()),
    }
}

impl MemoryProtocol {
    /// Handles one transport fetch. The body channel was installed synchronously by `load`;
    /// from then on EVERYTHING (also errors) travels through it.
    pub(super) async fn transport(
        &self,
        request: &mut Request,
        path: String,
        sender: UnboundedSender<Data>,
    ) -> Response {
        let terminal = Terminal::new(sender);
        let mut response = Response::new(
            request.current_url().clone(),
            ResourceFetchTiming::new(request.timing_type()),
        );
        let reply = match method_of(&request.method) {
            None => failure(405, "INVALID_ARGUMENT", "method not allowed"),
            Some(method) => {
                let body = match (method, request.body.take()) {
                    (Method::Post, Some(body)) => {
                        let limit = self.limits.max_bulk_body;
                        read_body(body, limit).await.map(Bytes::from)
                    }
                    _ => Ok(Bytes::new()),
                };
                match body {
                    Err(_) => failure(
                        400,
                        "INVALID_ARGUMENT",
                        "request body unreadable or too large",
                    ),
                    Ok(body) => {
                        let window = self.windows.resolve(request.target_webview_id);
                        let request = TransportRequest {
                            method,
                            path,
                            headers: collect_headers(&request.headers),
                            body,
                            window,
                            document: request.pipeline_id.map(document_of),
                        };
                        self.transport.handle(request).await
                    }
                }
            }
        };
        response.status = HttpStatus::new_raw(reply.status, vec![]);
        for (name, value) in &reply.headers {
            if let (Ok(name), Ok(value)) = (
                HeaderName::from_bytes(name.as_bytes()),
                HeaderValue::from_str(value),
            ) {
                response.headers.insert(name, value);
            }
        }
        *response.body.lock() = ServoBody::Receiving(Vec::new());
        self.handle.spawn(pump(terminal, reply.body));
        response
    }
}

/// Aborts the wrapped task when dropped: an interrupted fetch must not leave its command running.
pub(super) struct OwnedTask<T>(pub(super) JoinHandle<T>);

impl<T> Drop for OwnedTask<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

struct BodySession {
    body: RequestBody,
    sender: ipc::IpcSender<BodyChunkRequest>,
    complete: bool,
}

impl Drop for BodySession {
    fn drop(&mut self) {
        if !self.complete {
            let _ = self.sender.send(BodyChunkRequest::Error);
        }
        self.body.close_stream();
    }
}

/// Reads a whole request body, refusing more than `limit` bytes.
pub(super) async fn read_body(body: RequestBody, limit: usize) -> io::Result<Vec<u8>> {
    if body.len().is_some_and(|length| length > limit) {
        body.close_stream();
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Invocation body too large",
        ));
    }
    let sender = body
        .clone_stream()
        .lock()
        .clone()
        .ok_or_else(|| io::Error::other("Body stream closed"))?;
    let mut session = BodySession {
        body,
        sender,
        complete: false,
    };
    let (sender, receiver) = ipc::channel::<BodyChunkResponse>().map_err(io::Error::other)?;
    session
        .sender
        .send(BodyChunkRequest::Connect(sender))
        .map_err(io::Error::other)?;
    let mut stream = receiver.to_stream();
    let mut bytes = Vec::new();
    loop {
        session
            .sender
            .send(BodyChunkRequest::Chunk)
            .map_err(io::Error::other)?;
        match stream.next().await {
            Some(Ok(BodyChunkResponse::Chunk(chunk))) => {
                if chunk.len() > limit - bytes.len() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "Invocation body too large",
                    ));
                }
                bytes.extend_from_slice(&chunk);
            }
            Some(Ok(BodyChunkResponse::Done)) => {
                session.complete = true;
                return Ok(bytes);
            }
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Invocation body stream failed",
                ))
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alef_core::protocol::transport::TransportResponse;
    use std::time::Duration;
    use tokio::sync::mpsc::unbounded_channel;

    async fn collect(
        mut receiver: tokio::sync::mpsc::UnboundedReceiver<Data>,
    ) -> (Vec<usize>, &'static str) {
        let mut sizes = Vec::new();
        loop {
            match tokio::time::timeout(Duration::from_secs(10), receiver.recv())
                .await
                .expect("timed out")
            {
                Some(Data::Payload(bytes)) => sizes.push(bytes.len()),
                Some(Data::Done) => return (sizes, "done"),
                Some(Data::Cancelled) => return (sizes, "cancelled"),
                Some(_) => {}
                None => return (sizes, "closed without a terminal message"),
            }
        }
    }

    #[test]
    fn maps_only_the_two_transport_hosts_to_paths() {
        let path = |text: &str| transport_path(&Url::parse(text).expect("url"));
        assert_eq!(
            path("native://call/fs.read?x=1#f").as_deref(),
            Some("call/fs.read")
        );
        assert_eq!(path("native://stream/12").as_deref(), Some("stream/12"));
        assert_eq!(path("native://app/index.html"), None);
        assert_eq!(path("native://invoke/"), None);
        assert_eq!(path("native://calls/x"), None);
    }

    #[test]
    fn documents_are_ordered_by_pipeline_creation() {
        use servo_base::id::{Index, PipelineNamespaceId};
        use std::{marker::PhantomData, num::NonZeroU32};
        let pipeline = |namespace, index| PipelineId {
            namespace_id: PipelineNamespaceId(namespace),
            index: Index(NonZeroU32::new(index).expect("non-zero"), PhantomData),
        };
        assert!(document_of(pipeline(1, 1)) < document_of(pipeline(1, 2)));
        assert!(document_of(pipeline(1, 2)) < document_of(pipeline(1, 30)));
        assert_ne!(document_of(pipeline(1, 2)), document_of(pipeline(2, 2)));
    }

    #[test]
    fn only_get_post_and_options_reach_the_transport() {
        assert_eq!(method_of(&http::Method::GET), Some(Method::Get));
        assert_eq!(method_of(&http::Method::POST), Some(Method::Post));
        assert_eq!(method_of(&http::Method::OPTIONS), Some(Method::Options));
        assert_eq!(method_of(&http::Method::PUT), None);
        assert_eq!(method_of(&http::Method::DELETE), None);
    }

    #[test]
    fn undecodable_header_values_are_dropped() {
        let mut headers = http::HeaderMap::new();
        headers.insert("origin", HeaderValue::from_static("https://a.alef"));
        headers.insert(
            "authorization",
            HeaderValue::from_bytes(b"Bearer \xff").expect("opaque"),
        );
        assert_eq!(
            collect_headers(&headers),
            vec![("origin".to_owned(), "https://a.alef".to_owned())]
        );
    }

    #[tokio::test]
    async fn a_large_body_is_split_into_chunks_of_at_most_256_kib_and_terminated() {
        let (sender, receiver) = unbounded_channel();
        let body = ResponseBody::Bytes(Bytes::from(vec![1u8; 600 * 1024]));
        pump(Terminal::new(sender), body).await;
        let (sizes, end) = collect(receiver).await;
        assert_eq!(sizes, vec![256 * 1024, 256 * 1024, 88 * 1024]);
        assert_eq!(end, "done");
    }

    #[tokio::test]
    async fn an_empty_body_is_just_done() {
        let (sender, receiver) = unbounded_channel();
        pump(Terminal::new(sender), ResponseBody::Empty).await;
        assert_eq!(collect(receiver).await, (vec![], "done"));
    }

    #[tokio::test]
    async fn a_producer_that_vanishes_still_terminates_the_channel() {
        let (sender, receiver) = unbounded_channel();
        drop(Terminal::new(sender));
        assert_eq!(collect(receiver).await, (vec![], "cancelled"));
        let (sender, receiver) = unbounded_channel();
        let task = tokio::spawn(async move {
            let _terminal = Terminal::new(sender);
            std::future::pending::<()>().await;
        });
        tokio::task::yield_now().await;
        task.abort();
        assert_eq!(
            collect(receiver).await,
            (vec![], "cancelled"),
            "an aborted task is cancelled, not hung"
        );
    }

    #[tokio::test]
    async fn a_closed_receiver_stops_the_pump_without_panicking() {
        let (sender, receiver) = unbounded_channel();
        drop(receiver);
        let body = ResponseBody::Bytes(Bytes::from(vec![0u8; 1024 * 1024]));
        pump(Terminal::new(sender), body).await;
    }

    #[test]
    fn failure_responses_are_json_with_the_given_status() {
        let reply = failure(405, "INVALID_ARGUMENT", "method not allowed");
        assert_eq!(reply.status, 405);
        let TransportResponse {
            body: ResponseBody::Bytes(bytes),
            ..
        } = reply
        else {
            panic!("expected a buffered body");
        };
        let value: serde_json::Value = serde_json::from_slice(&bytes).expect("json");
        assert_eq!(value["code"], "INVALID_ARGUMENT");
    }
}
