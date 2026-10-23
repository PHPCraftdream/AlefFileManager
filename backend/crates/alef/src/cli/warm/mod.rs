// SPDX-License-Identifier: MIT OR Apache-2.0
//! The wiring of the warm start into the `alef` binary
//! (docs/stages/m11-warm-start.md, «Проводка в бинарнике alef (cli/warm)»):
//! the `broker` command (tray + IPC server + spare pool), the internal
//! `warm-spare` process, the `warm` control commands, and the warm client
//! attempt of `alef run` that falls back to the unchanged cold path.
pub(crate) mod broker;
pub(crate) mod client;
pub(crate) mod control;
pub(crate) mod endpoint;
pub(crate) mod env;
pub(crate) mod spare;

pub(crate) use client::try_run_warm;
