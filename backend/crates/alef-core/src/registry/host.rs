// SPDX-License-Identifier: MIT OR Apache-2.0
//! What modules may ask of the process that hosts the runtime (implemented by `alef-runtime`).
use serde::{Deserialize, Serialize};

/// The colour scheme of the desktop.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "lowercase")]
#[ts(export, export_to = "core.ts")]
pub enum Theme {
    Light,
    Dark,
}

/// The hosting process. Calls are cheap and never block on the UI thread.
pub trait Host: Send + Sync {
    /// Asks the event loop to finish; the process exits with `code` once the window is gone.
    fn quit(&self, code: i32);
    /// The current desktop theme; `Light` when the platform does not report one.
    fn theme(&self) -> Theme;
}
