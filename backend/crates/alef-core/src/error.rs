// SPDX-License-Identifier: MIT OR Apache-2.0
//! One error vocabulary for every API module; codes are part of the JS contract.
use std::fmt;
use std::io;

use serde::{Deserialize, Serialize};
use serde_json::Value;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
#[ts(export, export_to = "core.ts")]
pub enum ErrorCode {
    NotFound,
    AlreadyExists,
    NotADirectory,
    IsADirectory,
    DirectoryNotEmpty,
    PermissionDenied,
    InvalidArgument,
    Timeout,
    Closed,
    Busy,
    NotAvailable,
    ManifestInvalid,
    Internal,
}

impl ErrorCode {
    /// HTTP status carried by the transport response for this code.
    pub fn http_status(self) -> u16 {
        match self {
            Self::NotFound => 404,
            Self::AlreadyExists
            | Self::NotADirectory
            | Self::IsADirectory
            | Self::DirectoryNotEmpty => 409,
            Self::PermissionDenied => 403,
            Self::InvalidArgument | Self::ManifestInvalid => 400,
            Self::Timeout => 408,
            Self::Closed => 410,
            Self::Busy => 429,
            Self::NotAvailable => 501,
            Self::Internal => 500,
        }
    }

    pub fn as_str(self) -> &'static str {
        match self {
            Self::NotFound => "NOT_FOUND",
            Self::AlreadyExists => "ALREADY_EXISTS",
            Self::NotADirectory => "NOT_A_DIRECTORY",
            Self::IsADirectory => "IS_A_DIRECTORY",
            Self::DirectoryNotEmpty => "DIRECTORY_NOT_EMPTY",
            Self::PermissionDenied => "PERMISSION_DENIED",
            Self::InvalidArgument => "INVALID_ARGUMENT",
            Self::Timeout => "TIMEOUT",
            Self::Closed => "CLOSED",
            Self::Busy => "BUSY",
            Self::NotAvailable => "NOT_AVAILABLE",
            Self::ManifestInvalid => "MANIFEST_INVALID",
            Self::Internal => "INTERNAL",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = "core.ts")]
pub struct AlefError {
    pub code: ErrorCode,
    pub message: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(type = "unknown")]
    pub details: Option<Value>,
}

impl AlefError {
    pub fn new(code: ErrorCode, message: impl Into<String>) -> Self {
        Self {
            code,
            message: message.into(),
            details: None,
        }
    }

    pub fn with_details(mut self, details: Value) -> Self {
        self.details = Some(details);
        self
    }
}

impl fmt::Display for AlefError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.code.as_str(), self.message)
    }
}

impl std::error::Error for AlefError {}

impl From<io::Error> for AlefError {
    fn from(error: io::Error) -> Self {
        let code = match error.kind() {
            io::ErrorKind::NotFound => ErrorCode::NotFound,
            io::ErrorKind::AlreadyExists => ErrorCode::AlreadyExists,
            io::ErrorKind::NotADirectory => ErrorCode::NotADirectory,
            io::ErrorKind::IsADirectory => ErrorCode::IsADirectory,
            io::ErrorKind::DirectoryNotEmpty => ErrorCode::DirectoryNotEmpty,
            io::ErrorKind::ResourceBusy => ErrorCode::Busy,
            io::ErrorKind::PermissionDenied => ErrorCode::PermissionDenied,
            io::ErrorKind::InvalidInput | io::ErrorKind::InvalidData => ErrorCode::InvalidArgument,
            io::ErrorKind::TimedOut => ErrorCode::Timeout,
            io::ErrorKind::BrokenPipe
            | io::ErrorKind::ConnectionReset
            | io::ErrorKind::ConnectionAborted
            | io::ErrorKind::NotConnected
            | io::ErrorKind::UnexpectedEof => ErrorCode::Closed,
            io::ErrorKind::WouldBlock => ErrorCode::Busy,
            io::ErrorKind::Unsupported => ErrorCode::NotAvailable,
            _ => ErrorCode::Internal,
        };
        Self::new(code, error.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn io_kinds_map_to_codes() {
        let cases = [
            (io::ErrorKind::NotFound, ErrorCode::NotFound),
            (io::ErrorKind::NotADirectory, ErrorCode::NotADirectory),
            (io::ErrorKind::IsADirectory, ErrorCode::IsADirectory),
            (
                io::ErrorKind::DirectoryNotEmpty,
                ErrorCode::DirectoryNotEmpty,
            ),
            (io::ErrorKind::ResourceBusy, ErrorCode::Busy),
            (io::ErrorKind::PermissionDenied, ErrorCode::PermissionDenied),
            (io::ErrorKind::InvalidInput, ErrorCode::InvalidArgument),
            (io::ErrorKind::TimedOut, ErrorCode::Timeout),
            (io::ErrorKind::BrokenPipe, ErrorCode::Closed),
            (io::ErrorKind::WouldBlock, ErrorCode::Busy),
            (io::ErrorKind::Unsupported, ErrorCode::NotAvailable),
            (io::ErrorKind::Other, ErrorCode::Internal),
        ];
        for (kind, code) in cases {
            assert_eq!(
                AlefError::from(io::Error::from(kind)).code,
                code,
                "{kind:?}"
            );
        }
    }

    #[test]
    fn codes_serialize_as_screaming_snake_case_matching_as_str() {
        for code in [
            ErrorCode::NotFound,
            ErrorCode::AlreadyExists,
            ErrorCode::NotADirectory,
            ErrorCode::IsADirectory,
            ErrorCode::DirectoryNotEmpty,
            ErrorCode::PermissionDenied,
            ErrorCode::InvalidArgument,
            ErrorCode::Timeout,
            ErrorCode::Closed,
            ErrorCode::Busy,
            ErrorCode::NotAvailable,
            ErrorCode::ManifestInvalid,
            ErrorCode::Internal,
        ] {
            let json = serde_json::to_string(&code).expect("serialize");
            assert_eq!(json, format!("\"{}\"", code.as_str()));
        }
    }

    #[test]
    fn error_round_trips_and_omits_empty_details() {
        let error = AlefError::new(ErrorCode::PermissionDenied, "no");
        let json = serde_json::to_value(&error).expect("serialize");
        assert_eq!(
            json,
            serde_json::json!({"code": "PERMISSION_DENIED", "message": "no"})
        );
        let back: AlefError = serde_json::from_value(json).expect("deserialize");
        assert_eq!(back, error);
        let detailed = error.with_details(serde_json::json!({"path": "a"}));
        assert!(serde_json::to_value(&detailed)
            .expect("serialize")
            .get("details")
            .is_some());
    }

    #[test]
    fn generated_ts_error_code_union_matches_as_str_exactly() {
        let core = std::fs::read_to_string(
            std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
                .join("../../../packages/api/types/core.ts"),
        )
        .expect("generated types must exist; run `npm run gen:types`");
        let marker = "export type ErrorCode = ";
        let start = core.find(marker).expect("core.ts must declare ErrorCode");
        let end = start
            + core[start..]
                .find(';')
                .expect("ErrorCode union must end with ;");
        let mut members: Vec<&str> = core[start + marker.len()..end]
            .split('|')
            .map(|member| member.trim().trim_matches('"'))
            .collect();
        members.sort_unstable();
        let mut wire: Vec<&str> = [
            ErrorCode::NotFound,
            ErrorCode::AlreadyExists,
            ErrorCode::NotADirectory,
            ErrorCode::IsADirectory,
            ErrorCode::DirectoryNotEmpty,
            ErrorCode::PermissionDenied,
            ErrorCode::InvalidArgument,
            ErrorCode::Timeout,
            ErrorCode::Closed,
            ErrorCode::Busy,
            ErrorCode::NotAvailable,
            ErrorCode::ManifestInvalid,
            ErrorCode::Internal,
        ]
        .iter()
        .map(|code| code.as_str())
        .collect();
        wire.sort_unstable();
        assert_eq!(members, wire, "TS ErrorCode union must equal ErrorCode::as_str() over ALL variants, in both directions");
    }

    #[test]
    fn http_status_is_defined_for_every_code() {
        assert_eq!(ErrorCode::PermissionDenied.http_status(), 403);
        assert_eq!(ErrorCode::NotFound.http_status(), 404);
        assert_eq!(ErrorCode::ManifestInvalid.http_status(), 400);
        assert_eq!(ErrorCode::Internal.http_status(), 500);
    }
}
