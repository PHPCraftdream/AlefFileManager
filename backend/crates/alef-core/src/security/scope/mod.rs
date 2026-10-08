// SPDX-License-Identifier: MIT OR Apache-2.0
//! Fail-closed scope matching for filesystem paths, network targets and executables.
pub mod command;
pub(crate) mod exec;
pub(crate) mod net;
pub(crate) mod path;
pub mod sidecar;

use crate::{AlefError, ErrorCode};

pub(crate) use path::{canonical, canonical_entry};

/// Manifest error for an unusable scope pattern.
pub(crate) fn invalid(message: impl Into<String>) -> AlefError {
    AlefError::new(ErrorCode::ManifestInvalid, message.into())
}

/// Accepts non-empty text without NUL or other control characters.
pub(crate) fn clean(text: &str) -> bool {
    !text.is_empty() && !text.chars().any(char::is_control)
}
