// SPDX-License-Identifier: MIT OR Apache-2.0
//! Plain request/response types of the transport; no HTTP or Servo dependency.
use crate::{
    error::{AlefError, ErrorCode},
    protocol::frame::{encode, Frame},
    session::streams::StreamReader,
};
use bytes::Bytes;
use serde_json::Value;
use std::fmt;

/// Request methods the transport understands.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Method {
    /// Stream reads.
    Get,
    /// Unary calls.
    Post,
    /// CORS preflight.
    Options,
}

/// One request as seen by the transport; the glue fills it from the real HTTP request.
#[derive(Clone, Debug)]
pub struct TransportRequest {
    /// Request method.
    pub method: Method,
    /// `call/<command>` or `stream/<id>`; a leading `/` is tolerated.
    pub path: String,
    /// Header names are matched case-insensitively.
    pub headers: Vec<(String, String)>,
    /// Whole request body.
    pub body: Bytes,
    /// Opaque id of the window the request came from (derived by the glue from the webview).
    pub window: u64,
    /// Ordered id of the document that issued the request (derived by the glue from the page
    /// pipeline); `runtime.hello` ties the session to it. `None`: the glue cannot tell.
    pub document: Option<u64>,
}

impl TransportRequest {
    /// First header with this (case-insensitive) name.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

/// Response to hand back to the page.
#[derive(Debug)]
pub struct TransportResponse {
    /// HTTP status.
    pub status: u16,
    /// Lowercase header names.
    pub headers: Vec<(String, String)>,
    /// Response payload.
    pub body: ResponseBody,
}

impl TransportResponse {
    /// First header with this (case-insensitive) name.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(key, _)| key.eq_ignore_ascii_case(name))
            .map(|(_, value)| value.as_str())
    }
}

/// Response payload: nothing, a finished buffer, or a live frame stream.
pub enum ResponseBody {
    /// No body.
    Empty,
    /// Complete body; the glue splits it into chunks for the browser.
    Bytes(Bytes),
    /// Encoded stream frames, ending with a terminal frame.
    Frames(FrameBody),
}

impl fmt::Debug for ResponseBody {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Empty => f.write_str("Empty"),
            Self::Bytes(bytes) => write!(f, "Bytes({} bytes)", bytes.len()),
            Self::Frames(_) => f.write_str("Frames"),
        }
    }
}

/// Encoded frames of one outgoing stream.
pub struct FrameBody {
    reader: StreamReader,
    finished: bool,
}

impl FrameBody {
    pub(super) fn new(reader: StreamReader) -> Self {
        Self {
            reader,
            finished: false,
        }
    }

    /// Next encoded frame; `None` only after the terminal frame has been delivered.
    pub async fn next_chunk(&mut self) -> Option<Bytes> {
        if self.finished {
            return None;
        }
        match self.reader.next_frame().await {
            Some(frame) => {
                self.finished = matches!(frame, Frame::End | Frame::Error(_));
                Some(encode(&frame))
            }
            None => {
                self.finished = true;
                None
            }
        }
    }
}

fn response(status: u16, content_type: &str, body: ResponseBody) -> TransportResponse {
    TransportResponse {
        status,
        headers: vec![("content-type".into(), content_type.into())],
        body,
    }
}

/// 200 with a JSON body.
pub(super) fn json(value: &Value) -> TransportResponse {
    let bytes = serde_json::to_vec(value).unwrap_or_else(|_| b"null".to_vec());
    response(200, "application/json", ResponseBody::Bytes(bytes.into()))
}

/// 200 with an opaque binary body.
pub(super) fn octets(bytes: Bytes) -> TransportResponse {
    response(200, "application/octet-stream", ResponseBody::Bytes(bytes))
}

/// 200 with the frame stream.
pub(super) fn frames(body: FrameBody) -> TransportResponse {
    response(
        200,
        "application/vnd.alef.frames",
        ResponseBody::Frames(body),
    )
}

/// Error response: the status follows the code, the body is the serialized error.
pub(super) fn error(error: &AlefError) -> TransportResponse {
    with_status(error.code.http_status(), error)
}

/// Error response with an explicit status (e.g. 405).
pub(super) fn with_status(status: u16, error: &AlefError) -> TransportResponse {
    let bytes = serde_json::to_vec(error)
        .unwrap_or_else(|_| br#"{"code":"INTERNAL","message":"internal error"}"#.to_vec());
    response(
        status,
        "application/json",
        ResponseBody::Bytes(bytes.into()),
    )
}

/// The single denial every failed authentication or origin check maps to.
pub(super) fn denied() -> AlefError {
    AlefError::new(ErrorCode::PermissionDenied, "permission denied")
}

/// 204 answer to a preflight; the origin echo is added with the common headers.
pub(super) fn preflight() -> TransportResponse {
    TransportResponse {
        status: 204,
        headers: vec![
            (
                "access-control-allow-methods".into(),
                "GET, POST, OPTIONS".into(),
            ),
            (
                "access-control-allow-headers".into(),
                "authorization, content-type, x-alef-args".into(),
            ),
            ("access-control-max-age".into(), "600".into()),
        ],
        body: ResponseBody::Empty,
    }
}

/// Headers every response carries; the origin is echoed only when it passed the check.
pub(super) fn add_common_headers(response: &mut TransportResponse, allowed_origin: Option<&str>) {
    let headers = &mut response.headers;
    headers.push(("cache-control".into(), "no-store".into()));
    headers.push(("x-content-type-options".into(), "nosniff".into()));
    headers.push(("vary".into(), "origin".into()));
    if let Some(origin) = allowed_origin {
        headers.push(("access-control-allow-origin".into(), origin.into()));
    }
}
