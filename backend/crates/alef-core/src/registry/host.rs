// SPDX-License-Identifier: MIT OR Apache-2.0
//! What modules may ask of the process that hosts the runtime (implemented by `alef-runtime`).
use std::{future::Future, pin::Pin};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::window::UiCall;
use crate::AlefError;

/// The colour scheme of the desktop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "lowercase")]
#[ts(export, export_to = "core.ts")]
pub enum Theme {
    Light,
    Dark,
}

/// Answer that arrives once the thread owning the windows has done the work.
pub type HostFuture = Pin<Box<dyn Future<Output = Result<Value, AlefError>> + Send>>;

/// The hosting process. Calls are cheap and never block on the UI thread.
pub trait Host: Send + Sync {
    /// Asks the event loop to finish; the process exits with `code` once the window is gone.
    fn quit(&self, code: i32);
    /// The current desktop theme; `Light` when the platform does not report one.
    fn theme(&self) -> Theme;
    /// Runs `call` on the thread that owns the windows. `caller` is the window of the calling
    /// document (0 when there is none); the reply is the JSON the command returns.
    fn ui(&self, caller: u64, call: UiCall) -> HostFuture;
    /// Sends the event `name` to the documents of `window`, to those of every window when `None`.
    /// Never waits for the documents. Names that begin with `runtime.` belong to the runtime.
    fn emit(&self, window: Option<u64>, name: &str, payload: Value);
}
