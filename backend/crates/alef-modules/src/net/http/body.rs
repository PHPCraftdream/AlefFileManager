// SPDX-License-Identifier: MIT OR Apache-2.0
//! The bodies: what the page sends comes through a stream into the request, what the server sends goes
//! through a stream to the page. Both carry credit, so a slow side holds the other back and nothing
//! piles up in memory.
use std::io;

use alef_core::{
    session::streams::{IncomingReader, StreamWriter},
    AlefError, ErrorCode,
};
use bytes::Bytes;
use http_body_util::{channel::Channel, combinators::BoxBody, BodyExt, Empty, Full};
use hyper::{body::Incoming, Method, StatusCode};

pub(super) type RequestBody = BoxBody<Bytes, io::Error>;

/// What a request carries.
pub(super) enum Payload {
    Empty,
    /// In memory: a redirect that keeps the method sends it again.
    Bytes(Bytes),
    /// From the page, once: a redirect cannot send it again.
    Stream(Option<RequestBody>),
}

impl Payload {
    /// The body for one hop; a stream is spent by the first.
    pub(super) fn take(&mut self) -> RequestBody {
        match self {
            Self::Empty => Empty::new().map_err(|never| match never {}).boxed(),
            Self::Bytes(bytes) => Full::new(bytes.clone())
                .map_err(|never| match never {})
                .boxed(),
            Self::Stream(body) => body
                .take()
                .unwrap_or_else(|| Empty::new().map_err(|never| match never {}).boxed()),
        }
    }

    pub(super) fn replayable(&self) -> bool {
        !matches!(self, Self::Stream(_))
    }
}

/// Whether an answer has no body whatever the connection says: to a HEAD, and with these statuses.
pub(super) fn bodiless(method: &Method, status: StatusCode) -> bool {
    *method == Method::HEAD
        || matches!(
            status,
            StatusCode::NO_CONTENT | StatusCode::RESET_CONTENT | StatusCode::NOT_MODIFIED
        )
}

/// The body of a request that the page writes through a stream: pieces go to the connection as they
/// come, and a stream that fails fails the request.
pub(super) fn from_stream(mut reader: IncomingReader) -> RequestBody {
    let (mut sender, body) = Channel::<Bytes, io::Error>::new(2);
    tokio::spawn(async move {
        while let Some(piece) = reader.recv().await {
            match piece {
                Ok(piece) => {
                    if sender.send_data(piece).await.is_err() {
                        return;
                    }
                }
                Err(error) => {
                    sender.abort(io::Error::other(error.message));
                    return;
                }
            }
        }
    });
    body.boxed()
}

/// Sends the body of the answer to the page, frame by frame, as the page takes it. When the page
/// closes the stream (or the document goes away) the body is dropped and the connection with it.
pub(super) fn pump(mut body: Incoming, writer: StreamWriter) {
    tokio::spawn(async move {
        loop {
            match body.frame().await {
                None => {
                    writer.end();
                    return;
                }
                Some(Err(_)) => {
                    writer.error(AlefError::new(
                        ErrorCode::Network,
                        "the connection broke while the body came",
                    ));
                    return;
                }
                Some(Ok(frame)) => {
                    if let Ok(data) = frame.into_data() {
                        if writer.send_binary(data).await.is_err() {
                            return;
                        }
                    }
                }
            }
        }
    });
}

#[cfg(test)]
mod tests {
    use super::*;

    async fn collect(body: RequestBody) -> Vec<u8> {
        body.collect().await.unwrap().to_bytes().to_vec()
    }

    #[test]
    fn some_answers_have_no_body() {
        for (method, status) in [
            (Method::HEAD, 200),
            (Method::HEAD, 404),
            (Method::GET, 204),
            (Method::POST, 205),
            (Method::GET, 304),
        ] {
            assert!(
                bodiless(&method, StatusCode::from_u16(status).unwrap()),
                "{method} {status}"
            );
        }
        for (method, status) in [
            (Method::GET, 200),
            (Method::POST, 200),
            (Method::GET, 201),
            (Method::GET, 301),
            (Method::GET, 404),
            (Method::GET, 500),
            (Method::DELETE, 202),
        ] {
            assert!(
                !bodiless(&method, StatusCode::from_u16(status).unwrap()),
                "{method} {status}"
            );
        }
    }

    #[tokio::test]
    async fn a_body_in_memory_goes_again_and_a_stream_goes_once() {
        let mut empty = Payload::Empty;
        assert!(empty.replayable());
        assert!(collect(empty.take()).await.is_empty());

        let mut bytes = Payload::Bytes(Bytes::from_static(b"abc"));
        assert!(bytes.replayable());
        assert_eq!(collect(bytes.take()).await, b"abc");
        assert_eq!(collect(bytes.take()).await, b"abc");

        let piece = Full::new(Bytes::from_static(b"xyz"))
            .map_err(|never| match never {})
            .boxed();
        let mut stream = Payload::Stream(Some(piece));
        assert!(!stream.replayable());
        assert_eq!(collect(stream.take()).await, b"xyz");
        assert!(
            collect(stream.take()).await.is_empty(),
            "spent by the first hop"
        );
        assert!(!Payload::Stream(None).replayable());
    }
}
