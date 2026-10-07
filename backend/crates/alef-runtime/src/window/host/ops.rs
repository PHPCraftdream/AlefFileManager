// SPDX-License-Identifier: MIT OR Apache-2.0
//! What a document can do to one window (`WindowOp`), apart from closing it.
use std::io;

use alef_core::{
    registry::window::{
        geometry::{self, Axis},
        MonitorInfo, ResizeEdge, WindowOp,
    },
    AlefError,
};
use serde_json::Value;
use winit::dpi::{LogicalPosition, LogicalSize};
use winit::window::{Fullscreen, ResizeDirection, WindowLevel};

use super::displays::Displays;
use crate::window::state::State;

pub(super) fn io_error(error: AlefError) -> io::Error {
    use alef_core::ErrorCode;
    let kind = match error.code {
        ErrorCode::InvalidArgument | ErrorCode::ManifestInvalid => io::ErrorKind::InvalidInput,
        ErrorCode::NotAvailable => io::ErrorKind::Unsupported,
        ErrorCode::NotFound => io::ErrorKind::NotFound,
        ErrorCode::AlreadyExists => io::ErrorKind::AlreadyExists,
        ErrorCode::PermissionDenied => io::ErrorKind::PermissionDenied,
        _ => io::ErrorKind::Other,
    };
    io::Error::new(kind, error.message)
}

pub(super) fn direction(edge: ResizeEdge) -> ResizeDirection {
    match edge {
        ResizeEdge::North => ResizeDirection::North,
        ResizeEdge::NorthEast => ResizeDirection::NorthEast,
        ResizeEdge::East => ResizeDirection::East,
        ResizeEdge::SouthEast => ResizeDirection::SouthEast,
        ResizeEdge::South => ResizeDirection::South,
        ResizeEdge::SouthWest => ResizeDirection::SouthWest,
        ResizeEdge::West => ResizeDirection::West,
        ResizeEdge::NorthWest => ResizeDirection::NorthWest,
    }
}

/// A minimum may not exceed the maximum.
fn check_limits(minimum: Option<(f64, f64)>, maximum: Option<(f64, f64)>) -> io::Result<()> {
    match (minimum, maximum) {
        (Some(low), Some(high)) if low.0 > high.0 || low.1 > high.1 => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "The minimum window size exceeds the maximum",
        )),
        _ => Ok(()),
    }
}

fn denied(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::PermissionDenied, message)
}

/// Page zoom the API allows (Servo itself clamps to 0.1..=10).
const ZOOM_RANGE: std::ops::RangeInclusive<f64> = 0.25..=5.0;

impl State {
    /// The display the window is on, among all displays.
    fn display(&self) -> (Vec<MonitorInfo>, Option<usize>) {
        let displays = Displays::collect(
            self.window.available_monitors(),
            self.window.primary_monitor(),
        );
        let index = self
            .window
            .current_monitor()
            .and_then(|monitor| displays.at_origin(monitor.position()));
        (displays.infos(), index)
    }

    /// Brings a window that is outside its limits inside them.
    fn hold_to_limits(&self) {
        let (scale, size) = (self.window.scale_factor(), self.window.inner_size());
        let current = (
            f64::from(size.width) / scale,
            f64::from(size.height) / scale,
        );
        let held = geometry::clamp_size(current, self.min_size, self.max_size);
        if (held.0 - current.0).abs() >= 1.0 || (held.1 - current.1).abs() >= 1.0 {
            let _ = self
                .window
                .request_inner_size(LogicalSize::new(held.0, held.1));
        }
    }

    /// Does `op` to the window; `Close`, `Destroy` and the close interception belong to the host
    /// of all windows.
    pub(super) fn apply(&mut self, op: WindowOp) -> io::Result<Value> {
        let value = self.apply_to_window(op)?;
        self.snapshot_dirty = true;
        self.update_resize_cursor();
        Ok(value)
    }

    /// Quiet mode: the operations that would show or focus the system window only change what the
    /// document is told. `true` when `op` was one of them.
    fn pretend(&mut self, op: &WindowOp) -> bool {
        let Some(pretended) = self.quiet.as_mut() else {
            return false;
        };
        match op {
            WindowOp::Minimize => pretended.minimized = true,
            WindowOp::Maximize => {
                pretended.maximized = true;
                pretended.minimized = false;
            }
            WindowOp::Restore => {
                pretended.maximized = false;
                pretended.minimized = false;
            }
            WindowOp::ToggleMaximize => pretended.maximized = !pretended.maximized,
            WindowOp::SetFullscreen { enabled } => pretended.fullscreen = *enabled,
            WindowOp::Show => {
                pretended.visible = true;
                self.revealed = true;
            }
            WindowOp::Hide => {
                pretended.visible = false;
                self.revealed = true;
            }
            WindowOp::Focus => pretended.focused = true,
            _ => return false,
        }
        true
    }

    /// Maximizes or restores the window; for a window that is not shown yet, once it is.
    fn maximize(&mut self, maximized: bool) {
        self.window.set_maximized(maximized);
        if !self.revealed {
            self.maximize_when_shown = Some(maximized);
        }
    }

    fn apply_to_window(&mut self, op: WindowOp) -> io::Result<Value> {
        if self.pretend(&op) {
            return Ok(Value::Null);
        }
        match op {
            WindowOp::State => {
                return serde_json::to_value(self.fresh_snapshot()?).map_err(io::Error::other)
            }
            WindowOp::SetTitle { title } => {
                self.window.set_title(&title);
                self.title = title;
            }
            WindowOp::SetSize { width, height } => {
                let (infos, index) = self.display();
                let monitor = index.and_then(|index| infos.get(index));
                let wanted = geometry::size(width, height, monitor).map_err(io_error)?;
                let (width, height) = geometry::clamp_size(wanted, self.min_size, self.max_size);
                let _ = self
                    .window
                    .request_inner_size(LogicalSize::new(width, height));
            }
            WindowOp::SetPosition { x, y } => {
                let (infos, index) = self.display();
                let monitor = index.and_then(|index| infos.get(index));
                let x = geometry::coordinate(x, Axis::Horizontal, monitor).map_err(io_error)?;
                let y = geometry::coordinate(y, Axis::Vertical, monitor).map_err(io_error)?;
                self.window.set_outer_position(LogicalPosition::new(x, y));
            }
            WindowOp::Center => {
                let (infos, index) = self.display();
                let monitor = index.and_then(|index| infos.get(index)).ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::Unsupported,
                        "The window is on no known display",
                    )
                })?;
                let scale = self.window.scale_factor();
                let outer = self.window.outer_size();
                let at = geometry::center(
                    &monitor.work_area,
                    f64::from(outer.width) / scale,
                    f64::from(outer.height) / scale,
                );
                self.window
                    .set_outer_position(LogicalPosition::new(at.x, at.y));
            }
            WindowOp::Minimize => self.window.set_minimized(true),
            WindowOp::Maximize => self.maximize(true),
            WindowOp::Restore => {
                if self.window.is_minimized() == Some(true) {
                    self.window.set_minimized(false);
                }
                if self
                    .maximize_when_shown
                    .unwrap_or_else(|| self.window.is_maximized())
                {
                    self.maximize(false);
                }
            }
            WindowOp::ToggleMaximize => {
                let now = self
                    .maximize_when_shown
                    .unwrap_or_else(|| self.window.is_maximized());
                self.maximize(!now);
            }
            WindowOp::SetFullscreen { enabled } => self
                .window
                .set_fullscreen(enabled.then_some(Fullscreen::Borderless(None))),
            WindowOp::SetAlwaysOnTop { enabled } => {
                self.always_on_top = enabled;
                self.window.set_window_level(if enabled {
                    WindowLevel::AlwaysOnTop
                } else {
                    WindowLevel::Normal
                });
            }
            WindowOp::SetDecorations { enabled } => self.window.set_decorations(enabled),
            WindowOp::SetResizable { enabled } => self.window.set_resizable(enabled),
            WindowOp::SetMinSize { width, height } => {
                let (infos, index) = self.display();
                let monitor = index.and_then(|index| infos.get(index));
                let limit = geometry::limit(width, height, 0.0, monitor).map_err(io_error)?;
                check_limits(limit, self.max_size)?;
                self.min_size = limit;
                self.window
                    .set_min_inner_size(limit.map(|(w, h)| LogicalSize::new(w, h)));
                self.hold_to_limits();
            }
            WindowOp::SetMaxSize { width, height } => {
                let (infos, index) = self.display();
                let monitor = index.and_then(|index| infos.get(index));
                let limit = geometry::limit(width, height, geometry::UNLIMITED, monitor)
                    .map_err(io_error)?;
                check_limits(self.min_size, limit)?;
                self.max_size = limit;
                self.window
                    .set_max_inner_size(limit.map(|(w, h)| LogicalSize::new(w, h)));
                self.hold_to_limits();
            }
            WindowOp::Show => {
                self.revealed = true;
                self.window.set_visible(true);
            }
            WindowOp::Hide => {
                self.revealed = true;
                self.window.set_visible(false);
            }
            WindowOp::Focus => self.window.focus_window(),
            WindowOp::SetZoom { factor } => {
                if !ZOOM_RANGE.contains(&factor) {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "The zoom factor must be between 0.25 and 5",
                    ));
                }
                self.webview.set_page_zoom(factor as f32);
            }
            WindowOp::StartResize { .. } if !self.window.is_resizable() => {
                return Err(denied("Window resizing is disabled"));
            }
            WindowOp::StartDrag | WindowOp::StartResize { .. } if !self.primary_pressed => {
                return Err(denied("Drag requires a pressed primary mouse button"));
            }
            WindowOp::StartDrag => {
                self.window.drag_window().map_err(io::Error::other)?;
                self.primary_pressed = false;
            }
            WindowOp::StartResize { edge } => {
                self.window
                    .drag_resize_window(direction(edge))
                    .map_err(io::Error::other)?;
                self.native_resize_active = true;
                self.primary_pressed = false;
            }
            WindowOp::Close
            | WindowOp::Destroy
            | WindowOp::CloseIntercept { .. }
            | WindowOp::CloseAnswer { .. } => {
                return Err(io::Error::other(
                    "Closing is handled by the host of all windows",
                ))
            }
        }
        Ok(Value::Null)
    }
}
