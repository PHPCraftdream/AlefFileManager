// SPDX-License-Identifier: MIT OR Apache-2.0
//! Basic value types of the spare pool: slot kinds, warm level, spare ids,
//! slot states and the outward `Action` commands.

use core::time::Duration;

/// Logical kind of a spare slot. The broker decides the kind, never the client.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SlotKind {
    /// Hidden window + GL context.
    Windowed,
    /// Hidden WebView.
    Windowless,
}

impl SlotKind {
    /// Both slots, in a stable order.
    pub const ALL: [SlotKind; 2] = [SlotKind::Windowed, SlotKind::Windowless];

    pub(crate) fn index(self) -> usize {
        match self {
            SlotKind::Windowed => 0,
            SlotKind::Windowless => 1,
        }
    }
}

/// Warm level. Both levels keep the same two logical slots; the level only
/// says how far the spare process is pre-loaded before the handoff.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    /// Process skeleton: executable, crypto provider, tokio, waiting on IPC.
    Skeleton,
    /// Full warm: additionally the graphics context (Servo/GL or WebView).
    Full,
}

/// Identifier of a spare process; grows monotonically within one `Pool`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct SpareId(pub u64);

/// State of one slot (one per enabled kind).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SlotState {
    /// No process; a `fill` may spawn one immediately.
    Empty,
    /// A spare is being created; at most one per kind at any time.
    Initializing { id: SpareId, since: Duration },
    /// A spare is ready to be handed out exactly once.
    Ready { id: SpareId, since: Duration },
    /// The slot waits until `until` before refilling; `failures` counts
    /// consecutive failures (also used for the circuit-breaker pause and the
    /// pressure/restore pause).
    Backoff { until: Duration, failures: u32 },
}

/// A command for the wiring to execute outside the pool.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    /// Create a spare process of the given kind and level.
    Spawn {
        id: SpareId,
        kind: SlotKind,
        level: Level,
    },
    /// Terminate a spare process still owned by the pool.
    Kill { id: SpareId },
}

/// One slot of the pool; `failures` survives `Ready` so the backoff can grow
/// across a success boundary only when reset explicitly (on `on_ready`).
#[derive(Clone, Copy, Debug)]
pub(crate) struct Slot {
    pub kind: SlotKind,
    pub state: SlotState,
    pub failures: u32,
    /// Set by a severe memory pressure on the GL slot: refills only via
    /// `restore`, never by an ordinary `fill`.
    pub disabled: bool,
}

impl Slot {
    pub(crate) const fn new(kind: SlotKind) -> Self {
        Slot {
            kind,
            state: SlotState::Empty,
            failures: 0,
            disabled: false,
        }
    }

    pub(crate) fn id(&self) -> Option<SpareId> {
        match self.state {
            SlotState::Initializing { id, .. } | SlotState::Ready { id, .. } => Some(id),
            SlotState::Empty | SlotState::Backoff { .. } => None,
        }
    }
}
