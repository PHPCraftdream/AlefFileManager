// SPDX-License-Identifier: MIT OR Apache-2.0
//! Pure wire protocol of the warm-start broker: framing, messages, authentication,
//! request log, client state machine. No sockets, threads, processes, or real time —
//! time is always a parameter. Transport concerns live in `../ipc`.

pub mod auth;
pub mod client;
pub mod frame;
pub mod log;
pub mod message;
pub mod warm;
