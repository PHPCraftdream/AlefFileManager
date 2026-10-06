// SPDX-License-Identifier: MIT OR Apache-2.0
//! Incremental transport frame codec.
use std::fmt;

use bytes::{Bytes, BytesMut};
use serde_json::Value;

use crate::error::AlefError;

/// Maximum payload length accepted by [`FrameDecoder::new`].
pub const DEFAULT_MAX_PAYLOAD: usize = 16 * 1024 * 1024;

/// One transport frame; JSON frames contain a typed value and are serialized on encode.
#[derive(Clone, Debug, PartialEq)]
pub enum Frame {
    /// A JSON value.
    Json(Value),
    /// Raw binary data.
    Binary(Bytes),
    /// End of stream.
    End,
    /// A serialized transport error.
    Error(AlefError),
}

/// Serializes a frame; panics if its payload length does not fit in `u32`.
pub fn encode(frame: &Frame) -> Bytes {
    let (kind, payload) = match frame {
        Frame::Json(value) => (1, serde_json::to_vec(value).expect("serialize JSON value")),
        Frame::Binary(bytes) => (2, bytes.to_vec()),
        Frame::End => (3, Vec::new()),
        Frame::Error(error) => (4, serde_json::to_vec(error).expect("serialize AlefError")),
    };
    let len = u32::try_from(payload.len()).expect("frame payload must fit u32");
    let mut out = BytesMut::with_capacity(5 + payload.len());
    out.extend_from_slice(&[kind]);
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(&payload);
    out.freeze()
}

/// Frame decoding errors; a decoder is poisoned after any error.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum FrameError {
    /// Unrecognized frame kind.
    UnknownKind(u8),
    /// Declared payload exceeds configured maximum.
    TooLarge { declared: u32, max: usize },
    /// JSON payload is invalid.
    MalformedJson(String),
    /// Error payload or End frame payload is invalid.
    MalformedPayload(String),
    /// Decoder previously encountered an error.
    Poisoned,
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::UnknownKind(kind) => write!(f, "unknown frame kind {kind}"),
            Self::TooLarge { declared, max } => write!(f, "frame length {declared} exceeds {max}"),
            Self::MalformedJson(message) => write!(f, "malformed JSON frame: {message}"),
            Self::MalformedPayload(message) => write!(f, "malformed frame payload: {message}"),
            Self::Poisoned => f.write_str("frame decoder is poisoned"),
        }
    }
}

impl std::error::Error for FrameError {}

enum State {
    Header { buf: [u8; 5], filled: usize },
    Payload { kind: u8, len: usize, buf: Vec<u8> },
    Poisoned,
}

/// Incremental decoder tolerating arbitrary chunk boundaries.
pub struct FrameDecoder {
    max: usize,
    state: State,
}

impl FrameDecoder {
    /// Creates a decoder with [`DEFAULT_MAX_PAYLOAD`].
    pub fn new() -> Self {
        Self::with_max_payload(DEFAULT_MAX_PAYLOAD)
    }
    /// Creates a decoder with a maximum payload size; zero permits only empty payloads.
    pub fn with_max_payload(max: usize) -> Self {
        Self {
            max,
            state: State::Header {
                buf: [0; 5],
                filled: 0,
            },
        }
    }
    /// Returns the maximum accepted payload size.
    pub fn max_payload(&self) -> usize {
        self.max
    }

    /// Feeds arbitrary bytes and returns completed frames; errors permanently poison the decoder.
    pub fn push(&mut self, chunk: &[u8]) -> Result<Vec<Frame>, FrameError> {
        if matches!(self.state, State::Poisoned) {
            return Err(FrameError::Poisoned);
        }
        let mut out = Vec::new();
        let mut input = chunk;
        while !input.is_empty() {
            let state = std::mem::replace(&mut self.state, State::Poisoned);
            match state {
                State::Header {
                    mut buf,
                    mut filled,
                } => {
                    let take = (5 - filled).min(input.len());
                    buf[filled..filled + take].copy_from_slice(&input[..take]);
                    filled += take;
                    input = &input[take..];
                    if filled < 5 {
                        self.state = State::Header { buf, filled };
                        continue;
                    }
                    let kind = buf[0];
                    let declared = u32::from_le_bytes([buf[1], buf[2], buf[3], buf[4]]);
                    let err = if !(1..=4).contains(&kind) {
                        Some(FrameError::UnknownKind(kind))
                    } else if declared as u64 > self.max as u64 {
                        Some(FrameError::TooLarge {
                            declared,
                            max: self.max,
                        })
                    } else if kind == 3 && declared != 0 {
                        Some(FrameError::MalformedPayload(
                            "End frame payload must be empty".into(),
                        ))
                    } else {
                        None
                    };
                    if let Some(error) = err {
                        return self.fail(error);
                    }
                    if declared == 0 {
                        match decode_payload(kind, &[]) {
                            Ok(frame) => out.push(frame),
                            Err(error) => return self.fail(error),
                        }
                        self.state = State::Header {
                            buf: [0; 5],
                            filled: 0,
                        };
                    } else {
                        self.state = State::Payload {
                            kind,
                            len: declared as usize,
                            buf: Vec::with_capacity((declared as usize).min(64 * 1024)),
                        };
                    }
                }
                State::Payload { kind, len, mut buf } => {
                    let take = (len - buf.len()).min(input.len());
                    buf.extend_from_slice(&input[..take]);
                    input = &input[take..];
                    if buf.len() == len {
                        match decode_payload(kind, &buf) {
                            Ok(frame) => out.push(frame),
                            Err(error) => return self.fail(error),
                        }
                        self.state = State::Header {
                            buf: [0; 5],
                            filled: 0,
                        };
                    } else {
                        self.state = State::Payload { kind, len, buf };
                    }
                }
                State::Poisoned => return Err(FrameError::Poisoned),
            }
        }
        Ok(out)
    }

    fn fail<T>(&mut self, error: FrameError) -> Result<T, FrameError> {
        self.state = State::Poisoned;
        Err(error)
    }
}

impl Default for FrameDecoder {
    fn default() -> Self {
        Self::new()
    }
}

fn decode_payload(kind: u8, bytes: &[u8]) -> Result<Frame, FrameError> {
    match kind {
        1 => serde_json::from_slice(bytes)
            .map(Frame::Json)
            .map_err(|e| FrameError::MalformedJson(e.to_string())),
        2 => Ok(Frame::Binary(Bytes::copy_from_slice(bytes))),
        3 if bytes.is_empty() => Ok(Frame::End),
        3 => Err(FrameError::MalformedPayload(
            "End frame payload must be empty".into(),
        )),
        4 => serde_json::from_slice(bytes)
            .map(Frame::Error)
            .map_err(|e| FrameError::MalformedPayload(e.to_string())),
        other => Err(FrameError::UnknownKind(other)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn boundary_and_degenerate_payloads() {
        assert_eq!(DEFAULT_MAX_PAYLOAD, 16 * 1024 * 1024);
        let boundary = Frame::Binary(Bytes::from(vec![7; 16]));
        assert_eq!(
            FrameDecoder::with_max_payload(16)
                .push(&encode(&boundary))
                .expect("boundary"),
            vec![boundary]
        );
        assert_eq!(
            FrameDecoder::with_max_payload(16)
                .push(&encode(&Frame::Binary(Bytes::from(vec![7; 17])))),
            Err(FrameError::TooLarge {
                declared: 17,
                max: 16
            })
        );
        let empty = Frame::Binary(Bytes::new());
        assert_eq!(
            FrameDecoder::new().push(&encode(&empty)).expect("empty"),
            vec![empty]
        );
        assert_eq!(
            FrameDecoder::with_max_payload(0)
                .push(&encode(&Frame::End))
                .expect("end"),
            vec![Frame::End]
        );
        assert_eq!(
            FrameDecoder::with_max_payload(0)
                .push(&encode(&Frame::Binary(Bytes::from_static(&[1])))),
            Err(FrameError::TooLarge {
                declared: 1,
                max: 0
            })
        );
    }

    #[test]
    fn round_trip_all_frames() {
        let frames = vec![
            Frame::Json(serde_json::json!({"s":"привет", "n":[1,2.5]})),
            Frame::Binary(Bytes::from_static(&[1, 0, 2, 255])),
            Frame::End,
            Frame::Error(AlefError::new(crate::error::ErrorCode::Busy, "busy")),
            Frame::Error(
                AlefError::new(crate::error::ErrorCode::Internal, "bad")
                    .with_details(serde_json::json!({"x":1})),
            ),
        ];
        for frame in frames {
            assert_eq!(
                FrameDecoder::new().push(&encode(&frame)).expect("decode"),
                vec![frame]
            );
        }
    }
    #[test]
    fn boundaries_chunks_and_concatenation() {
        for frame in [
            Frame::Json(serde_json::json!({"a":1})),
            Frame::Binary(Bytes::from_static(&[1, 0, 2])),
            Frame::End,
            Frame::Error(
                AlefError::new(crate::error::ErrorCode::Closed, "gone")
                    .with_details(serde_json::json!({"k": [1, 2]})),
            ),
        ] {
            let bytes = encode(&frame);
            // every split point, including 0 (all in the second push) and len (all in the first)
            for i in 0..=bytes.len() {
                let mut d = FrameDecoder::new();
                let first = d.push(&bytes[..i]).expect("first part");
                assert_eq!(first.is_empty(), i < bytes.len(), "split at {i}");
                let mut got = first;
                got.extend(d.push(&bytes[i..]).expect("rest"));
                assert_eq!(got, vec![frame.clone()], "split at {i}");
            }
            let mut d = FrameDecoder::new();
            let mut got = Vec::new();
            for byte in bytes.iter() {
                got.extend(d.push(std::slice::from_ref(byte)).expect("byte"));
            }
            assert_eq!(got, vec![frame]);
        }
        let frames = vec![
            Frame::Binary(Bytes::from_static(&[2])),
            Frame::Json(serde_json::json!(true)),
            Frame::End,
        ];
        let mut joined = Vec::new();
        for frame in &frames {
            joined.extend_from_slice(&encode(frame));
        }
        let bytes = Bytes::from(joined);
        assert_eq!(FrameDecoder::new().push(&bytes).expect("decode"), frames);
    }
    #[test]
    fn rejects_bad_headers_payloads_and_poisoning() {
        let mut d = FrameDecoder::with_max_payload(8);
        assert_eq!(
            d.push(&[2, 9, 0, 0, 0]),
            Err(FrameError::TooLarge {
                declared: 9,
                max: 8
            })
        );
        assert_eq!(d.push(&[]), Err(FrameError::Poisoned));
        let mut d = FrameDecoder::new();
        assert_eq!(d.push(&[7, 0, 0, 0, 0]), Err(FrameError::UnknownKind(7)));
        for (wire, expected_json) in [
            (
                vec![
                    1, 8, 0, 0, 0, b'n', b'o', b't', b' ', b'j', b's', b'o', b'n',
                ],
                true,
            ),
            (vec![4, 2, 0, 0, 0, b'{', b'}'], false),
            (vec![3, 1, 0, 0, 0], false),
        ] {
            let err = FrameDecoder::new()
                .push(&wire)
                .expect_err("invalid payload");
            assert!(if expected_json {
                matches!(err, FrameError::MalformedJson(_))
            } else {
                matches!(err, FrameError::MalformedPayload(_))
            });
        }
    }
}
