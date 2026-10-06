// SPDX-License-Identifier: MIT OR Apache-2.0
use std::io;

use windows_sys::Win32::Foundation::POINT;
use windows_sys::Win32::Graphics::Gdi::{GetMonitorInfoW, MONITORINFO};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    GetCursorPos, SystemParametersInfoW, SPI_GETWHEELSCROLLCHARS, SPI_GETWHEELSCROLLLINES,
};
use winit::dpi::PhysicalPosition;
use winit::monitor::MonitorHandle;
use winit::platform::windows::{MonitorHandleExtWindows, WindowAttributesExtWindows};
use winit::window::{Icon, WindowAttributes};

use super::{PhysicalRect, ScrollAxis};

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

/// The part of the display that the task bar does not take.
pub(crate) fn work_area(monitor: &MonitorHandle) -> Option<PhysicalRect> {
    // SAFETY: an all-zero MONITORINFO is valid plain data.
    let mut info: MONITORINFO = unsafe { std::mem::zeroed() };
    info.cbSize = std::mem::size_of::<MONITORINFO>() as u32;
    // SAFETY: `info` is live with its size set; the monitor handle comes from winit.
    let found = unsafe { GetMonitorInfoW(monitor.hmonitor(), &mut info) };
    if found == 0 {
        return None;
    }
    let area = info.rcWork;
    Some(PhysicalRect {
        x: area.left,
        y: area.top,
        width: u32::try_from(area.right - area.left).ok()?,
        height: u32::try_from(area.bottom - area.top).ok()?,
    })
}

/// The pointer on the virtual desktop.
pub(crate) fn cursor_position() -> Option<PhysicalPosition<i32>> {
    let mut point = POINT { x: 0, y: 0 };
    // SAFETY: `point` is a live POINT that the call fills.
    let found = unsafe { GetCursorPos(&mut point) };
    (found != 0).then(|| PhysicalPosition::new(point.x, point.y))
}
