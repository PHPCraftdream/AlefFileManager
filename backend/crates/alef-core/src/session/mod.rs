// SPDX-License-Identifier: MIT OR Apache-2.0
#![allow(clippy::module_inception)]
pub mod resources;
pub mod session;
pub mod streams;

/// Session-owned resource interface and table.
pub use resources::{Resource, ResourceTable};
/// Session lifecycle and call permit types.
pub use session::{CallPermit, Session, SessionManager, TokenSource};
/// Bidirectional stream hub and endpoints.
pub use streams::{IncomingReader, IncomingWriter, StreamHub, StreamReader, StreamWriter};
