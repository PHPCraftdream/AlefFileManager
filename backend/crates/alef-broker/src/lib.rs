// SPDX-License-Identifier: MIT OR Apache-2.0
//! The broker of the warm start: the pool of spare processes, the wire protocol and its
//! transport, the autostart of the broker. The design is `docs/stages/m11-warm-start.md`.
#![deny(unsafe_op_in_unsafe_fn)]

pub mod autostart;
pub mod broker;
pub mod ipc;
pub mod peer;
pub mod pool;
pub mod protocol;
