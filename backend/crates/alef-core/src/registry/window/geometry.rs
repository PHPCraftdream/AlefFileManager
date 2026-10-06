// SPDX-License-Identifier: MIT OR Apache-2.0
//! Window geometry of FRAMEWORK-PLAN §6.2: lengths in px, `%screen` and `%work` resolved against a
//! display. Pure arithmetic in logical pixels; the process that owns the windows supplies the displays.
use serde::{Deserialize, Serialize};

use super::{MonitorInfo, Point, Rect};
use crate::{
    security::window::{Length, LengthUnit, Monitor, WindowDef, WindowPosition},
    AlefError, ErrorCode,
};

/// Stand-in for "no upper limit" where a finite size is needed.
pub const UNLIMITED: f64 = 1.0e6;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Axis {
    Horizontal,
    Vertical,
}

/// Where a window opens and how big it is, in logical pixels.
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct Placement {
    pub width: f64,
    pub height: f64,
    /// Top-left of the outer frame; `None` leaves the choice to the system.
    pub position: Option<Point>,
    pub min_size: Option<(f64, f64)>,
    pub max_size: Option<(f64, f64)>,
}

fn invalid(message: &str) -> AlefError {
    AlefError::new(ErrorCode::InvalidArgument, message)
}

fn along(rect: &Rect, axis: Axis) -> (f64, f64) {
    match axis {
        Axis::Horizontal => (rect.x, rect.width),
        Axis::Vertical => (rect.y, rect.height),
    }
}

fn area(monitor: &MonitorInfo, unit: LengthUnit) -> &Rect {
    match unit {
        LengthUnit::Screen => &monitor.bounds,
        LengthUnit::Work => &monitor.work_area,
    }
}

fn require(monitor: Option<&MonitorInfo>) -> Result<&MonitorInfo, AlefError> {
    monitor.ok_or_else(|| {
        AlefError::new(
            ErrorCode::NotAvailable,
            "no display is available to resolve a percentage",
        )
    })
}

/// The display a window is placed on: the primary one, or the one under the cursor (the primary one
/// when the cursor is unknown or on no display). No primary flag means the first display.
pub fn pick(
    monitors: &[MonitorInfo],
    choice: Monitor,
    cursor: Option<Point>,
) -> Option<&MonitorInfo> {
    let primary = monitors
        .iter()
        .find(|monitor| monitor.primary)
        .or_else(|| monitors.first());
    match (choice, cursor) {
        (Monitor::Cursor, Some(point)) => monitors
            .iter()
            .find(|monitor| monitor.bounds.contains(point))
            .or(primary),
        _ => primary,
    }
}

/// A size along `axis`: pixels as given, percentages of the display (`%screen`) or of its work area.
pub fn extent(length: Length, axis: Axis, monitor: Option<&MonitorInfo>) -> Result<f64, AlefError> {
    match length {
        Length::Px(pixels) => Ok(pixels),
        Length::Percent(percent, unit) => {
            Ok(along(area(require(monitor)?, unit), axis).1 * percent / 100.0)
        }
    }
}

/// A coordinate along `axis`: pixels are desktop coordinates, a percentage is counted from the
/// origin of the display it refers to (`%screen`) or of its work area (`%work`).
pub fn coordinate(
    length: Length,
    axis: Axis,
    monitor: Option<&MonitorInfo>,
) -> Result<f64, AlefError> {
    match length {
        Length::Px(pixels) => Ok(pixels),
        Length::Percent(percent, unit) => {
            let (origin, size) = along(area(require(monitor)?, unit), axis);
            Ok(origin + size * percent / 100.0)
        }
    }
}

/// A window size from two lengths; both must be positive.
pub fn size(
    width: Length,
    height: Length,
    monitor: Option<&MonitorInfo>,
) -> Result<(f64, f64), AlefError> {
    let width = extent(width, Axis::Horizontal, monitor)?;
    let height = extent(height, Axis::Vertical, monitor)?;
    if !(width.is_finite() && height.is_finite() && width >= 1.0 && height >= 1.0) {
        return Err(invalid(
            "a window size must be at least 1 px in both directions",
        ));
    }
    Ok((width, height))
}

/// A minimum or maximum size; a side that is not given gets `missing`. `None` when neither is.
pub fn limit(
    width: Option<Length>,
    height: Option<Length>,
    missing: f64,
    monitor: Option<&MonitorInfo>,
) -> Result<Option<(f64, f64)>, AlefError> {
    if width.is_none() && height.is_none() {
        return Ok(None);
    }
    let side = |length: Option<Length>, axis| match length {
        Some(length) => extent(length, axis, monitor),
        None => Ok(missing),
    };
    Ok(Some((
        side(width, Axis::Horizontal)?,
        side(height, Axis::Vertical)?,
    )))
}

/// Top-left that centres a `width` x `height` window in `area`; a window larger than the area keeps
/// its top-left corner on the area.
pub fn center(area: &Rect, width: f64, height: f64) -> Point {
    Point {
        x: area.x + ((area.width - width) / 2.0).max(0.0),
        y: area.y + ((area.height - height) / 2.0).max(0.0),
    }
}

/// `size` held between the limits; a limit that is not set does not hold anything back.
pub fn clamp_size(
    size: (f64, f64),
    min_size: Option<(f64, f64)>,
    max_size: Option<(f64, f64)>,
) -> (f64, f64) {
    let (minimum, maximum) = (
        min_size.unwrap_or((0.0, 0.0)),
        max_size.unwrap_or((UNLIMITED, UNLIMITED)),
    );
    (
        size.0.max(minimum.0).min(maximum.0),
        size.1.max(minimum.1).min(maximum.1),
    )
}

/// Resolves a window definition (manifest or `window.create`) against the displays: size clamped
/// to the limits, position as asked (`center` is the middle of the work area of the chosen display).
pub fn place(
    definition: &WindowDef,
    monitors: &[MonitorInfo],
    cursor: Option<Point>,
) -> Result<Placement, AlefError> {
    let monitor = pick(monitors, definition.monitor, cursor);
    let min_size = limit(definition.min_width, definition.min_height, 0.0, monitor)?;
    let max_size = limit(
        definition.max_width,
        definition.max_height,
        UNLIMITED,
        monitor,
    )?;
    let (minimum, maximum) = (
        min_size.unwrap_or((0.0, 0.0)),
        max_size.unwrap_or((UNLIMITED, UNLIMITED)),
    );
    if minimum.0 > maximum.0 || minimum.1 > maximum.1 {
        return Err(invalid("the minimum window size exceeds the maximum"));
    }
    let wanted = size(definition.width, definition.height, monitor)?;
    let (width, height) = clamp_size(wanted, min_size, max_size);
    let position = match (&definition.position, monitor) {
        (WindowPosition::Center, Some(monitor)) => Some(center(&monitor.work_area, width, height)),
        (WindowPosition::Center, None) => None,
        (WindowPosition::At { x, y }, _) => Some(Point {
            x: coordinate(*x, Axis::Horizontal, monitor)?,
            y: coordinate(*y, Axis::Vertical, monitor)?,
        }),
    };
    Ok(Placement {
        width,
        height,
        position,
        min_size,
        max_size,
    })
}

/// Where a window was when the application last ran: its outer position and client size in
/// logical pixels as `window.state()` reports them, and whether it was maximized. The position is
/// unknown where the system does not tell it (Wayland).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct Remembered {
    pub x: Option<f64>,
    pub y: Option<f64>,
    pub width: f64,
    pub height: f64,
    pub maximized: bool,
}

/// How much of a window must lie in a work area for a person to still get hold of it.
const GRAB_WIDTH: f64 = 100.0;
const GRAB_HEIGHT: f64 = 40.0;
/// How far the top edge of a window may stick out above a work area.
const TOP_SLACK: f64 = 16.0;

/// Whether the window at `rect` can still be reached: its top edge is in a work area and enough
/// of it is there to be grabbed.
pub fn reachable(rect: &Rect, monitors: &[MonitorInfo]) -> bool {
    monitors.iter().any(|monitor| {
        let area = &monitor.work_area;
        let wide = (rect.x + rect.width).min(area.x + area.width) - rect.x.max(area.x);
        let high = (rect.y + rect.height).min(area.y + area.height) - rect.y.max(area.y);
        wide >= GRAB_WIDTH.min(rect.width)
            && high >= GRAB_HEIGHT.min(rect.height)
            && rect.y >= area.y - TOP_SLACK
    })
}

/// The window `definition` describes, opened where it was last time: the remembered size (never
/// larger than the biggest work area, so a smaller display leaves no window half off screen) and,
/// when the place is still on a display, the remembered position. A place that is on no display any
/// more leaves the placement of the definition (the centre of its display) as it is.
pub fn restore(
    definition: &WindowDef,
    remembered: &Remembered,
    monitors: &[MonitorInfo],
) -> WindowDef {
    let widest = monitors
        .iter()
        .map(|m| m.work_area.width)
        .fold(0.0, f64::max);
    let tallest = monitors
        .iter()
        .map(|m| m.work_area.height)
        .fold(0.0, f64::max);
    let cap = |wanted: f64, most: f64| if most > 0.0 { wanted.min(most) } else { wanted };
    let (width, height) = (
        cap(remembered.width, widest),
        cap(remembered.height, tallest),
    );
    let mut restored = definition.clone();
    restored.width = Length::Px(width);
    restored.height = Length::Px(height);
    if let (Some(x), Some(y)) = (remembered.x, remembered.y) {
        let rect = Rect {
            x,
            y,
            width,
            height,
        };
        if reachable(&rect, monitors) {
            restored.position = WindowPosition::At {
                x: Length::Px(x),
                y: Length::Px(y),
            };
        }
    }
    restored
}
