// SPDX-License-Identifier: MIT OR Apache-2.0
//! A granted-but-not-yet-accepted launch: the client is answered only when
//! the spare reports `Started`, declines, dies or the deadline passes.

use std::time::Duration;

use crate::ipc::ConnWriter;
use crate::protocol::message::RequestId;

use super::spawner::{SpareId, SpareProcess};

/// Covers the spare's full pre-start path including a consent window waiting
/// for the user.
pub(super) const ASSIGN_TIMEOUT: Duration = Duration::from_secs(600);

/// A launch granted to a spare whose acceptance is still pending: `Assign`
/// was sent, but the spare has not yet reported `Started`. The detached
/// client writers answer the final `Accepted`/`Declined` and (after
/// acceptance) carry the output relay.
pub(super) struct PendingLaunch {
    pub(super) rid: RequestId,
    pub(super) spare_id: SpareId,
    pub(super) process: Box<dyn SpareProcess>,
    /// Final answer + (after acceptance) the `Exited` relay writer.
    pub(super) writer: Box<dyn ConnWriter + Send>,
    pub(super) writer_out: Box<dyn ConnWriter + Send>,
    pub(super) writer_err: Box<dyn ConnWriter + Send>,
    /// Duplicates of `Launch` with the same rid received while pending; they
    /// share the launch's outcome.
    pub(super) extra: Vec<Box<dyn ConnWriter + Send>>,
    /// Grant time, for the [`ASSIGN_TIMEOUT`] deadline.
    pub(super) since: Duration,
}
