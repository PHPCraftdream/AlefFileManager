// SPDX-License-Identifier: MIT OR Apache-2.0
//! Peer libraries of the warm start (docs/stages/m11-warm-start.md,
//! «Архитектура реализации»): thin wrappers over [`crate::ipc::Client`] for
//! the two non-broker roles of the `alef` executable.
//!
//! - [`spare`] — the `alef warm-spare` process: handshake, wait for the
//!   assignment, report ready or decline.
//! - [`client`] — `alef run`: try a warm launch through the broker, fall
//!   back to cold on a proven decline, never blind-duplicate on ambiguity.
//!
//! Shared: the identity a local peer presents in `Hello` (this crate's build
//! plus the exact values or absence of the store-identity variables of the
//! current process — the same rule the broker uses at construction).

pub mod client;
pub mod spare;
#[cfg(test)]
mod tests;

use std::ffi::OsString;

use crate::protocol::message::{Hello, OsBytes, PROTOCOL_VERSION};

/// `Hello` for a local peer of THIS process: build identity of the crate and
/// the exact store-identity environment (absence is a value). The broker's
/// `ExpectedIdentity` wiring must use the same build string.
pub(crate) fn own_hello() -> Hello {
    Hello {
        protocol_version: PROTOCOL_VERSION,
        build: env!("CARGO_PKG_VERSION").to_string(),
        alef_home: std::env::var_os("ALEF_HOME").map(|v: OsString| OsBytes::from_os_string(&v)),
        alef_app_home: std::env::var_os("ALEF_APP_HOME")
            .map(|v: OsString| OsBytes::from_os_string(&v)),
    }
}
