// SPDX-License-Identifier: MIT OR Apache-2.0
//! Unary-call argument parsing and transport limits.
use super::credit::{DEFAULT_CHUNK_SIZE, DEFAULT_STREAM_WINDOW};
use crate::error::{AlefError, ErrorCode};
use serde_json::Value;

/// Maximum decoded size of the `x-alef-args` payload.
pub const MAX_ARGS_DECODED: usize = 64 * 1024;

/// Parses percent-encoded JSON ASCII; plus remains literal, escapes must be valid, and decoded UTF-8 is required.
pub fn parse_args_header(value: &str) -> Result<Value, AlefError> {
    let invalid = || AlefError::new(ErrorCode::InvalidArgument, "invalid x-alef-args");
    if value.is_empty() {
        return Err(invalid());
    }
    let input = value.as_bytes();
    let mut decoded = Vec::with_capacity(input.len().min(MAX_ARGS_DECODED));
    let mut i = 0;
    while i < input.len() {
        let byte = if input[i] == b'%' {
            if i + 2 >= input.len() {
                return Err(invalid());
            }
            let hi = hex(input[i + 1]).ok_or_else(invalid)?;
            let lo = hex(input[i + 2]).ok_or_else(invalid)?;
            i += 3;
            (hi << 4) | lo
        } else {
            let b = input[i];
            if !b.is_ascii() {
                return Err(invalid());
            }
            i += 1;
            b
        };
        if decoded.len() == MAX_ARGS_DECODED {
            return Err(invalid());
        }
        decoded.push(byte);
    }
    let text = std::str::from_utf8(&decoded).map_err(|_| invalid())?;
    serde_json::from_str(text).map_err(|_| invalid())
}
fn hex(b: u8) -> Option<u8> {
    match b {
        b'0'..=b'9' => Some(b - b'0'),
        b'a'..=b'f' => Some(b - b'a' + 10),
        b'A'..=b'F' => Some(b - b'A' + 10),
        _ => None,
    }
}

/// Per-session transport limits.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Limits {
    /// Maximum resources per session.
    pub max_resources_per_session: usize,
    /// Maximum simultaneous calls per session.
    pub max_concurrent_calls_per_session: usize,
    /// Maximum unary request body size.
    pub max_unary_body: usize,
    /// Maximum bulk request body size.
    pub max_bulk_body: usize,
    /// Stream flow-control window.
    pub stream_window: usize,
    /// Stream send chunk size.
    pub chunk_size: usize,
}
impl Default for Limits {
    fn default() -> Self {
        Self {
            max_resources_per_session: 1024,
            max_concurrent_calls_per_session: 32,
            max_unary_body: 256 * 1024,
            max_bulk_body: 128 * 1024 * 1024,
            stream_window: DEFAULT_STREAM_WINDOW,
            chunk_size: DEFAULT_CHUNK_SIZE,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn encoded_form_within_cap_ok() {
        let payload = format!("\"{}\"", "a".repeat(MAX_ARGS_DECODED - 2));
        let encoded = payload
            .bytes()
            .map(|byte| format!("%{byte:02X}"))
            .collect::<String>();
        assert_eq!(encoded.len(), MAX_ARGS_DECODED * 3);
        assert!(parse_args_header(&encoded).is_ok());
    }

    #[test]
    fn parses_and_rejects_header_values() {
        for s in [
            "{}",
            "{\"a\":1,\"b\":[true,null]}",
            "{\"a\":\"b+c\"}",
            "{\"x\":\"%D0%BF%D1%80%D0%B8%D0%B2%D0%B5%D1%82\"}",
        ] {
            assert!(parse_args_header(s).is_ok(), "{s}");
        }
        for s in ["%", "%1", "%GG", "%%%", "%2", "привет", "%FF", ""] {
            assert_eq!(
                parse_args_header(s).expect_err("invalid").code,
                ErrorCode::InvalidArgument
            );
        }
    }
    #[test]
    fn size_limit_and_depth() {
        let exact = format!("\"{}\"", "a".repeat(MAX_ARGS_DECODED - 2));
        assert!(parse_args_header(&exact).is_ok());
        let large = format!("\"{}\"", "a".repeat(MAX_ARGS_DECODED - 1));
        assert!(parse_args_header(&large).is_err());
        let nested = format!("{}0{}", "[".repeat(200), "]".repeat(200));
        assert!(parse_args_header(&nested).is_err());
    }
    #[test]
    fn limits_contract() {
        assert_eq!(
            Limits::default(),
            Limits {
                max_resources_per_session: 1024,
                max_concurrent_calls_per_session: 32,
                max_unary_body: 256 * 1024,
                max_bulk_body: 128 * 1024 * 1024,
                stream_window: DEFAULT_STREAM_WINDOW,
                chunk_size: DEFAULT_CHUNK_SIZE
            }
        );
    }
}
