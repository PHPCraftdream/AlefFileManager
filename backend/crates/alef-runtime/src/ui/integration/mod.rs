// SPDX-License-Identifier: MIT OR Apache-2.0
//! UI-thread integrations. The spike retains its own receiver while explicitly enabled.
pub(crate) mod forward;
#[cfg(any(windows, target_os = "macos"))]
pub(crate) mod menu;
#[cfg(not(any(windows, target_os = "macos")))]
#[path = "menu/unavailable.rs"]
pub(crate) mod menu;
pub(crate) mod shortcut;

pub(crate) fn spike_owns_hotkeys() -> bool {
    cfg!(feature = "spike-integration")
        && std::env::var("ALEF_SPIKE_INTEGRATION").is_ok_and(|value| value == "1")
}
