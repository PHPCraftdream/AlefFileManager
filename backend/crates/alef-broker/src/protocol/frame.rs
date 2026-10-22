// SPDX-License-Identifier: MIT OR Apache-2.0
//! Length-prefix framing over a byte stream, plus backpressure primitives.
//!
//! A frame is a `u32` little-endian body length followed by that many body bytes.
//! Decoding is pure: bytes are pushed in arbitrary chunks, time is never read.

use std::error::Error;
use std::fmt;

/// Hard cap on a single frame body, bytes.
pub const MAX_FRAME_LEN: u32 = 1 << 20; // 1 MiB

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FrameError {
    /// Peer declared a body longer than `MAX_FRAME_LEN`.
    TooLarge { declared: u32 },
    /// Zero-length frames carry no message and are rejected.
    ZeroLength,
    /// Stream ended in the middle of a frame.
    Truncated { expected: u32, got: usize },
    /// Caller tried to encode a body longer than `MAX_FRAME_LEN`.
    BodyTooLarge { len: usize },
}

impl fmt::Display for FrameError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            FrameError::TooLarge { declared } => {
                write!(
                    f,
                    "declared frame length {declared} exceeds {MAX_FRAME_LEN}"
                )
            }
            FrameError::ZeroLength => write!(f, "zero-length frame"),
            FrameError::Truncated { expected, got } => {
                write!(
                    f,
                    "truncated frame: expected {expected} body bytes, got {got}"
                )
            }
            FrameError::BodyTooLarge { len } => {
                write!(f, "body of {len} bytes exceeds frame cap {MAX_FRAME_LEN}")
            }
        }
    }
}

impl Error for FrameError {}

/// Encode one frame: 4-byte LE length header + body.
pub fn encode_frame(body: &[u8]) -> Result<Vec<u8>, FrameError> {
    if body.is_empty() {
        return Err(FrameError::ZeroLength);
    }
    let len =
        u32::try_from(body.len()).map_err(|_| FrameError::BodyTooLarge { len: body.len() })?;
    if len > MAX_FRAME_LEN {
        return Err(FrameError::BodyTooLarge { len: body.len() });
    }
    let mut out = Vec::with_capacity(4 + body.len());
    out.extend_from_slice(&len.to_le_bytes());
    out.extend_from_slice(body);
    Ok(out)
}

/// Incremental frame decoder for a byte stream.
///
/// Feed arbitrary chunks to [`FrameDecoder::push`]; completed frame bodies are
/// returned in order. Nothing proportional to the *declared* length is ever
/// allocated before the bytes actually arrive, so a malicious header of
/// `0xFFFFFFFF` fails without a large allocation.
///
/// After a [`FrameError::TooLarge`] error the decoder does not know where the
/// peer's real body ends, so the stream is unsynchronized. This is by design,
/// fail-closed: the connection must be dropped, not resumed.
#[derive(Debug, Default)]
pub struct FrameDecoder {
    // Buffered header+body bytes of the frame currently in progress.
    buf: Vec<u8>,
}

impl FrameDecoder {
    pub fn new() -> Self {
        Self::default()
    }

    /// Feed arbitrary bytes; returns bodies of frames completed by this push,
    /// in order. On any error the decoder state is unspecified — drop the
    /// connection (see type docs).
    pub fn push(&mut self, bytes: &[u8]) -> Result<Vec<Vec<u8>>, FrameError> {
        let mut done = Vec::new();
        let mut rest = bytes;
        loop {
            if self.buf.len() < 4 {
                // Still collecting the header: the buffer never grows past 4 here.
                let take = rest.len().min(4 - self.buf.len());
                self.buf.extend_from_slice(&rest[..take]);
                rest = &rest[take..];
                if self.buf.len() < 4 {
                    break;
                }
            }
            let declared = u32::from_le_bytes([self.buf[0], self.buf[1], self.buf[2], self.buf[3]]);
            if declared > MAX_FRAME_LEN {
                // Fail before any allocation for the body.
                return Err(FrameError::TooLarge { declared });
            }
            if declared == 0 {
                return Err(FrameError::ZeroLength);
            }
            let body_len = declared as usize;
            let have = self.buf.len() - 4;
            if have < body_len {
                // Append only what actually arrived; capacity grows with real
                // bytes, never with the declared length.
                let take = rest.len().min(body_len - have);
                self.buf.extend_from_slice(&rest[..take]);
                rest = &rest[take..];
                if self.buf.len() - 4 < body_len {
                    break;
                }
            }
            self.buf.drain(..4);
            let body: Vec<u8> = self.buf.drain(..body_len).collect();
            done.push(body);
        }
        Ok(done)
    }

    /// Call at stream close: errors if a partial frame was buffered.
    pub fn finish(self) -> Result<(), FrameError> {
        if self.buf.is_empty() {
            Ok(())
        } else if self.buf.len() < 4 {
            Err(FrameError::Truncated {
                expected: 4,
                got: self.buf.len(),
            })
        } else {
            let declared = u32::from_le_bytes([self.buf[0], self.buf[1], self.buf[2], self.buf[3]]);
            Err(FrameError::Truncated {
                expected: declared,
                got: self.buf.len() - 4,
            })
        }
    }

    /// Test-only view of the internal buffer capacity.
    #[cfg(test)]
    pub(crate) fn test_buffer_capacity(&self) -> usize {
        self.buf.capacity()
    }
}

/// Backpressure limits for a broker connection.
#[derive(Debug, Clone)]
pub struct Limits {
    pub max_concurrent_requests: u32,
    pub max_queue: usize,
    pub frames_per_second: u32,
}

impl Default for Limits {
    fn default() -> Self {
        Limits {
            max_concurrent_requests: 8,
            max_queue: 64,
            frames_per_second: 200,
        }
    }
}

/// Token bucket over an injected millisecond clock (no real time).
#[derive(Debug, Clone)]
pub struct TokenBucket {
    capacity: u32,
    refill_per_sec: u32,
    tokens: u32,
    last_refill_ms: Option<u64>,
}

impl TokenBucket {
    /// `capacity` and `refill_per_sec` are clamped to at least 1.
    pub fn new(capacity: u32, refill_per_sec: u32) -> Self {
        TokenBucket {
            capacity: capacity.max(1),
            refill_per_sec: refill_per_sec.max(1),
            tokens: capacity.max(1),
            last_refill_ms: None,
        }
    }

    /// Refill from `now_ms` (saturating, never exceeding capacity) and consume
    /// one token if available. The first call starts the clock and allows.
    pub fn allow(&mut self, now_ms: u64) -> bool {
        match self.last_refill_ms {
            None => {
                self.last_refill_ms = Some(now_ms);
            }
            Some(last) => {
                let elapsed_ms = now_ms.saturating_sub(last);
                // Refill, saturating both the product and the capacity.
                let gained = (elapsed_ms.min(u32::MAX as u64) as u32)
                    .saturating_mul(self.refill_per_sec)
                    / 1000;
                self.tokens = self.capacity.min(self.tokens.saturating_add(gained));
                self.last_refill_ms = Some(now_ms);
            }
        }
        if self.tokens > 0 {
            self.tokens -= 1;
            true
        } else {
            false
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip() {
        let body = b"hello broker";
        let bytes = encode_frame(body).unwrap();
        let mut dec = FrameDecoder::new();
        let frames = dec.push(&bytes).unwrap();
        assert_eq!(frames, vec![body.to_vec()]);
        dec.finish().unwrap();
    }

    #[test]
    fn byte_at_a_time() {
        let bytes = encode_frame(b"one").unwrap();
        let bytes2 = encode_frame(b"two-two").unwrap();
        let mut data = bytes;
        data.extend_from_slice(&bytes2);
        let mut dec = FrameDecoder::new();
        let mut frames = Vec::new();
        for chunk in data.chunks(1) {
            frames.extend(dec.push(chunk).unwrap());
        }
        assert_eq!(frames, vec![b"one".to_vec(), b"two-two".to_vec()]);
        dec.finish().unwrap();
    }

    #[test]
    fn arbitrary_boundaries() {
        let body: Vec<u8> = (0..64u8).collect();
        let data = encode_frame(&body).unwrap();
        for split in [2usize, 5, 4, 20, 40] {
            let mut dec = FrameDecoder::new();
            let mut frames = dec.push(&data[..split]).unwrap();
            assert!(frames.is_empty());
            frames.extend(dec.push(&data[split..]).unwrap());
            assert_eq!(frames, vec![body.clone()], "split at {split}");
            dec.finish().unwrap();
        }
    }

    #[test]
    fn two_frames_one_push() {
        let mut data = encode_frame(b"a").unwrap();
        data.extend_from_slice(&encode_frame(b"bbbb").unwrap());
        let mut dec = FrameDecoder::new();
        let frames = dec.push(&data).unwrap();
        assert_eq!(frames, vec![b"a".to_vec(), b"bbbb".to_vec()]);
    }

    #[test]
    fn huge_declared_length_no_allocation() {
        let mut dec = FrameDecoder::new();
        let err = dec.push(&0xFFFFFFFFu32.to_le_bytes()).unwrap_err();
        assert_eq!(err, FrameError::TooLarge { declared: u32::MAX });
        assert!(dec.test_buffer_capacity() < 4096);
    }

    #[test]
    fn zero_length_frame() {
        let mut dec = FrameDecoder::new();
        assert_eq!(
            dec.push(&0u32.to_le_bytes()).unwrap_err(),
            FrameError::ZeroLength
        );
    }

    #[test]
    fn truncated_header_only() {
        let mut dec = FrameDecoder::new();
        assert!(dec.push(&[0x03, 0x00]).unwrap().is_empty());
        assert_eq!(
            dec.finish().unwrap_err(),
            FrameError::Truncated {
                expected: 4,
                got: 2
            }
        );
    }

    #[test]
    fn truncated_body() {
        let mut dec = FrameDecoder::new();
        assert!(dec.push(&10u32.to_le_bytes()).unwrap().is_empty());
        assert!(dec.push(&[1, 2, 3, 4]).unwrap().is_empty());
        assert_eq!(
            dec.finish().unwrap_err(),
            FrameError::Truncated {
                expected: 10,
                got: 4
            }
        );
    }

    #[test]
    fn encode_rejects_empty_and_oversized() {
        assert_eq!(encode_frame(b"").unwrap_err(), FrameError::ZeroLength);
        assert!(matches!(
            encode_frame(&vec![0u8; MAX_FRAME_LEN as usize + 1]),
            Err(FrameError::BodyTooLarge { .. })
        ));
    }

    #[test]
    fn token_bucket_burst_and_refill() {
        let mut tb = TokenBucket::new(3, 3);
        assert!(tb.allow(0));
        assert!(tb.allow(0));
        assert!(tb.allow(0));
        assert!(!tb.allow(0));
        // One second later: 3 tokens refilled (capacity-capped).
        assert!(tb.allow(1000));
        assert!(tb.allow(1000));
        assert!(tb.allow(1000));
        assert!(!tb.allow(1000));
        // Huge timestamps must not panic or overflow.
        assert!(tb.allow(u64::MAX));
    }

    #[test]
    fn token_bucket_partial_refill() {
        let mut tb = TokenBucket::new(10, 100);
        for _ in 0..10 {
            assert!(tb.allow(0));
        }
        assert!(!tb.allow(0));
        assert!(tb.allow(50)); // 100 * 0.05s = 5 tokens
        assert!(tb.allow(50));
        assert!(tb.allow(50));
        assert!(tb.allow(50));
        assert!(tb.allow(50));
        assert!(!tb.allow(50));
    }
}
