// SPDX-License-Identifier: MIT OR Apache-2.0
//! The network modules (`http`, `socket`) through the registry, against a server on the loopback.
#[path = "../common/mod.rs"]
mod common;

mod http;
mod server;
mod shared;
mod socket;
mod websocket;
