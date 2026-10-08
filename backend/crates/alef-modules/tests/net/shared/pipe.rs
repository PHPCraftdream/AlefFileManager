// SPDX-License-Identifier: MIT OR Apache-2.0
//! A stream of the runtime read as a page reads it: every frame is acknowledged.
use std::{sync::Arc, time::Duration};

use alef_core::{ids::StreamId, protocol::frame::Frame, session::streams::StreamReader, AlefError};
use serde_json::Value;

use crate::common::Fixture;

/// A stream that comes from the runtime, read as a page reads it: every frame is acknowledged.
pub struct Pipe {
    app: Arc<alef_core::session::session::Session>,
    id: StreamId,
    reader: StreamReader,
    pub pending: Vec<u8>,
}

impl Pipe {
    pub fn open(app: &Fixture, id: u64) -> Self {
        let session = app.session();
        let id = StreamId(id);
        let reader = session.streams().reader(id).expect("an outgoing stream");
        Self {
            app: session,
            id,
            reader,
            pending: Vec::new(),
        }
    }

    pub async fn frame(&mut self) -> Option<Frame> {
        tokio::time::timeout(Duration::from_secs(20), self.reader.next_frame())
            .await
            .expect("the stream stalled")
    }

    /// The next bytes, once there are `count` of them; the surplus waits for the next call.
    pub async fn exactly(&mut self, count: usize) -> Vec<u8> {
        while self.pending.len() < count {
            match self.frame().await {
                Some(Frame::Binary(bytes)) => {
                    self.app.streams().ack(self.id, bytes.len()).unwrap();
                    self.pending.extend_from_slice(&bytes);
                }
                other => panic!(
                    "the stream ended after {} bytes: {other:?}",
                    self.pending.len()
                ),
            }
        }
        self.pending.drain(..count).collect()
    }

    /// Everything until the stream ends: the bytes, or the error it ended with.
    pub async fn until_end(&mut self) -> Result<Vec<u8>, AlefError> {
        loop {
            match self.frame().await {
                Some(Frame::Binary(bytes)) => {
                    self.app.streams().ack(self.id, bytes.len()).unwrap();
                    self.pending.extend_from_slice(&bytes);
                }
                Some(Frame::End) | None => return Ok(std::mem::take(&mut self.pending)),
                Some(Frame::Error(error)) => return Err(error),
                Some(Frame::Json(value)) => panic!("unexpected {value}"),
            }
        }
    }

    /// The next frame of JSON (connections that come, datagrams that arrive), `None` at the end.
    pub async fn json(&mut self) -> Option<Value> {
        match self.frame().await {
            Some(Frame::Json(value)) => {
                self.app
                    .streams()
                    .ack(self.id, value.to_string().len())
                    .unwrap();
                Some(value)
            }
            Some(Frame::End) | None => None,
            Some(Frame::Error(error)) => panic!("the stream failed: {error}"),
            Some(Frame::Binary(_)) => panic!("unexpected bytes"),
        }
    }
}
