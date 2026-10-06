// SPDX-License-Identifier: MIT OR Apache-2.0
use super::{PhysicalRect, ScrollAxis, DEFAULT_SCROLL_UNITS};
use std::io;
use winit::dpi::PhysicalPosition;
use winit::monitor::MonitorHandle;
use winit::window::{Icon, WindowAttributes};

pub(crate) const SUPPORTS_NATIVE_RESIZE: bool = true;

pub(crate) fn apply_window_icon(attributes: WindowAttributes, icon: Icon) -> WindowAttributes {
    attributes.with_window_icon(Some(icon))
}

// Winit supplies compositor wheel units; the common layer applies Servo's reference scale.
pub(crate) fn wheel_scroll_units(_: ScrollAxis) -> io::Result<u32> {
    Ok(DEFAULT_SCROLL_UNITS)
}

/// The part of the display that panels do not take; unknown here, so the whole display counts.
pub(crate) fn work_area(_: &MonitorHandle) -> Option<PhysicalRect> {
    None
}

/// The pointer on the desktop; this platform does not tell.
pub(crate) fn cursor_position() -> Option<PhysicalPosition<i32>> {
    None
}
