// SPDX-License-Identifier: MIT OR Apache-2.0
//! Manifest, permissions, scopes and CSP (M1.3), and what the user gave on top (M2b).
pub mod csp;
mod given;
pub mod manifest;
pub mod permissions;
pub(crate) mod scope;
pub mod window;

pub use given::{consent, grants};
pub use scope::{command, sidecar};
