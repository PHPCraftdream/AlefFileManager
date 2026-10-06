// SPDX-License-Identifier: MIT OR Apache-2.0
//! Framework core: transport protocol, sessions, resources, command registry, security.

pub mod error;
pub mod ids;
pub mod protocol;
pub mod registry;
pub mod security;
pub mod session;

pub use error::{AlefError, ErrorCode};
pub use ids::{ResourceId, SessionId, StreamId};

pub const PROTOCOL_VERSION: u32 = 1;
