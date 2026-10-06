// SPDX-License-Identifier: MIT OR Apache-2.0
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
mod portable;
#[cfg(target_os = "windows")]
mod windows;

#[derive(Clone, Copy)]
pub(crate) enum ScrollAxis {
    Horizontal,
    Vertical,
}
pub(crate) const DEFAULT_SCROLL_UNITS: u32 = 3;

#[cfg(target_os = "linux")]
pub(crate) use linux::{apply_window_icon, wheel_scroll_units, SUPPORTS_NATIVE_RESIZE};
#[cfg(target_os = "macos")]
pub(crate) use macos::{apply_window_icon, wheel_scroll_units, SUPPORTS_NATIVE_RESIZE};
#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
pub(crate) use portable::{apply_window_icon, wheel_scroll_units, SUPPORTS_NATIVE_RESIZE};
#[cfg(target_os = "windows")]
pub(crate) use windows::{apply_window_icon, wheel_scroll_units, SUPPORTS_NATIVE_RESIZE};
