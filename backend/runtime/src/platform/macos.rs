// SPDX-License-Identifier: GPL-3.0-or-later
use super::{ScrollAxis, DEFAULT_SCROLL_UNITS};
use std::io;
use winit::window::{Icon, WindowAttributes};

pub(crate) const SUPPORTS_NATIVE_RESIZE: bool = false;

pub(crate) fn apply_window_icon(attributes: WindowAttributes, icon: Icon) -> WindowAttributes {
    attributes.with_window_icon(Some(icon))
}

// Precision trackpad pixels bypass this line-wheel policy.
pub(crate) fn wheel_scroll_units(_: ScrollAxis) -> io::Result<u32> {
    Ok(DEFAULT_SCROLL_UNITS)
}
