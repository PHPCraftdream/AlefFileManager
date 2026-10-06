// SPDX-License-Identifier: MIT OR Apache-2.0
use std::io;

use windows_sys::Win32::UI::WindowsAndMessaging::{
    SystemParametersInfoW, SPI_GETWHEELSCROLLCHARS, SPI_GETWHEELSCROLLLINES,
};
use winit::platform::windows::WindowAttributesExtWindows;
use winit::window::{Icon, WindowAttributes};

use super::ScrollAxis;

pub(crate) const SUPPORTS_NATIVE_RESIZE: bool = true;

pub(crate) fn apply_window_icon(attributes: WindowAttributes, icon: Icon) -> WindowAttributes {
    attributes
        .with_taskbar_icon(Some(icon.clone()))
        .with_window_icon(Some(icon))
}

pub(crate) fn wheel_scroll_units(axis: ScrollAxis) -> io::Result<u32> {
    let action = match axis {
        ScrollAxis::Horizontal => SPI_GETWHEELSCROLLCHARS,
        ScrollAxis::Vertical => SPI_GETWHEELSCROLLLINES,
    };
    let mut units = 0u32;
    // SAFETY: these GET actions write one UINT to this aligned, live u32; no pointer is retained.
    let success = unsafe { SystemParametersInfoW(action, 0, (&mut units as *mut u32).cast(), 0) };
    if success == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(units)
}
