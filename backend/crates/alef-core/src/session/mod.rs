// SPDX-License-Identifier: MIT OR Apache-2.0
#![allow(clippy::module_inception)]
pub mod executor;
pub mod resources;
pub mod session;
pub mod streams;
#[cfg(any(test, feature = "e2e"))]
pub mod watchdog;

pub use executor::{BlockingExecutor, BlockingJob};
/// Session-owned resource interface and table.
pub use resources::{Resource, ResourceTable};
/// Session lifecycle and call permit types.
pub use session::{CallPermit, Session, SessionManager, TokenSource};
/// Bidirectional stream hub and endpoints.
pub use streams::{IncomingReader, IncomingWriter, StreamHub, StreamReader, StreamWriter};
