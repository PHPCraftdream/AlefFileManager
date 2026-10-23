// SPDX-License-Identifier: MIT OR Apache-2.0
//! Broker core of the warm start (docs/stages/m11-warm-start.md,
//! «Архитектура реализации»): one authenticated IPC endpoint serves clients,
//! spares and admin; the broker validates launches, hands out pool spares,
//! relays spare output/exit to the client and answers queries/admin.

mod broker;
mod pending;
mod spawner;
#[cfg(test)]
mod tests;

pub use broker::Broker;
pub use spawner::{ExitInfo, ProcessSpawner, SpareId, SpareProcess, Spawner};
