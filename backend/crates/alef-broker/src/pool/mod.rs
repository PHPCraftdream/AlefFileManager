// SPDX-License-Identifier: MIT OR Apache-2.0
//! Slice of the warm start (docs/stages/m11-warm-start.md): the spare-process
//! pool as a pure finite-state machine. Two logical slots at most, one per
//! kind (`Windowed`, `Windowless`); a granted spare is one-shot and a
//! replacement of the same kind is scheduled in the same operation. All
//! external effects (spawn, kill) are returned as [`Action`] commands; time
//! and randomness come from the injected [`Clock`] and [`Jitter`] traits, so
//! every behaviour is testable with fakes. The pool knows nothing about
//! processes, IPC or application binding.
//!
//! Entry points: [`Pool::fill`] to reach the target state (the wiring calls
//! it on events and at [`Pool::next_wake`]), [`Pool::take`] for an atomic
//! handoff, and the event inputs `on_ready` / `on_failed` / `on_exited`,
//! `release_pressure` / `restore`, `shutdown`. Any miss (empty slot, lost
//! race, failed replacement) degrades to a cold start without blocking.

mod backoff;
mod config;
mod pool;
mod state;
#[cfg(test)]
mod tests;

pub use backoff::{Clock, Jitter, SystemClock};
pub use config::PoolConfig;
pub use pool::{Pool, Pressure, Take};
pub use state::{Action, Level, SlotKind, SlotState, SpareId};
