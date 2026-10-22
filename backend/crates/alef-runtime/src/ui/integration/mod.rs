// SPDX-License-Identifier: MIT OR Apache-2.0
//! UI-thread integrations. The spike retains its own receiver while explicitly enabled.
pub(crate) mod forward;
#[cfg(any(windows, target_os = "macos"))]
pub(crate) mod menu;
#[cfg(not(any(windows, target_os = "macos")))]
#[path = "menu/unavailable.rs"]
pub(crate) mod menu;
pub(crate) mod shortcut;
mod shortcut_source;
// Platform-independent tray logic (the decoder shared with the native backends, the
// muda-free menu plan and the SNI mapping for the Linux backend) plus its tests.
#[cfg(any(windows, target_os = "macos"))]
pub(crate) mod tray;
#[cfg(target_os = "linux")]
#[path = "tray/linux/mod.rs"]
pub(crate) mod tray;
#[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
#[path = "tray/unavailable.rs"]
pub(crate) mod tray;
#[path = "tray/pure.rs"]
pub(crate) mod tray_pure;

pub(crate) fn spike_owns_hotkeys() -> bool {
    cfg!(feature = "spike-integration")
        && std::env::var("ALEF_SPIKE_INTEGRATION").is_ok_and(|value| value == "1")
}
