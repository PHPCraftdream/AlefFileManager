// SPDX-License-Identifier: MIT OR Apache-2.0
//! Platform integration renderers and the internal installer contract.

mod native;
mod render;
mod transaction;
#[cfg(all(windows, not(any(test, feature = "e2e"))))]
mod windows;
pub(crate) use native::NativeIntegration;
#[allow(unused_imports)]
pub use render::{desktop_entry, mac_launcher, plist, safe_display_component, windows_arguments};

use std::{io, path::PathBuf};

/// Information needed to render or create an application integration.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct Artifact {
    pub id: String,
    pub name: String,
    pub hash: String,
    pub executable: PathBuf,
    pub package: PathBuf,
    pub icon: Option<PathBuf>,
}

/// Implementations must undo every artifact they created if `install` returns an error.
/// Kept internal until platform mutation semantics are implemented. `Send` so the updater
/// applier can hold one behind a mutex across threads.
pub(crate) trait Integration: Send {
    fn install(&mut self, identity: &Artifact) -> io::Result<()>;
    fn remove(&mut self, id: &str, name: Option<&str>) -> io::Result<bool>;
}
