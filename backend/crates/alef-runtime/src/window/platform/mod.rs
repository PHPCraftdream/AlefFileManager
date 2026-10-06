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

/// A rectangle in physical pixels of the virtual desktop.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct PhysicalRect {
    pub x: i32,
    pub y: i32,
    pub width: u32,
    pub height: u32,
}

#[cfg(target_os = "linux")]
pub(crate) use linux::{
    apply_window_icon, cursor_position, wheel_scroll_units, work_area, SUPPORTS_NATIVE_RESIZE,
};
#[cfg(target_os = "macos")]
pub(crate) use macos::{
    apply_window_icon, cursor_position, wheel_scroll_units, work_area, SUPPORTS_NATIVE_RESIZE,
};
#[cfg(not(any(target_os = "windows", target_os = "macos", target_os = "linux")))]
pub(crate) use portable::{
    apply_window_icon, cursor_position, wheel_scroll_units, work_area, SUPPORTS_NATIVE_RESIZE,
};
#[cfg(target_os = "windows")]
pub(crate) use windows::{
    apply_window_icon, cursor_position, wheel_scroll_units, work_area, SUPPORTS_NATIVE_RESIZE,
};
