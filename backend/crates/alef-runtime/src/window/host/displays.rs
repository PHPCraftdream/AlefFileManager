// SPDX-License-Identifier: MIT OR Apache-2.0
//! The displays as the window API sees them: logical pixels, each display in its own scale (see
//! `MonitorInfo`), and the way back to the physical pixels the system wants.
use alef_core::registry::window::{MonitorInfo, Point, Rect};
use winit::dpi::{PhysicalPosition, PhysicalSize};
use winit::monitor::MonitorHandle;

use crate::window::platform::{self, PhysicalRect};

struct Display {
    info: MonitorInfo,
    /// Top-left in physical pixels of the virtual desktop.
    origin: PhysicalPosition<i32>,
}

pub(super) struct Displays(Vec<Display>);

fn logical(rect: PhysicalRect, scale: f64) -> Rect {
    Rect {
        x: f64::from(rect.x) / scale,
        y: f64::from(rect.y) / scale,
        width: f64::from(rect.width) / scale,
        height: f64::from(rect.height) / scale,
    }
}

impl Displays {
    pub(super) fn collect(
        monitors: impl Iterator<Item = MonitorHandle>,
        primary: Option<MonitorHandle>,
    ) -> Self {
        Self(
            monitors
                .map(|monitor| {
                    let scale = Some(monitor.scale_factor())
                        .filter(|scale| scale.is_finite() && *scale > 0.0)
                        .unwrap_or(1.0);
                    let (position, size) = (monitor.position(), monitor.size());
                    let whole = PhysicalRect {
                        x: position.x,
                        y: position.y,
                        width: size.width,
                        height: size.height,
                    };
                    let work = platform::work_area(&monitor).unwrap_or(whole);
                    Display {
                        info: MonitorInfo {
                            name: monitor.name(),
                            bounds: logical(whole, scale),
                            work_area: logical(work, scale),
                            scale_factor: scale,
                            primary: primary.as_ref() == Some(&monitor),
                        },
                        origin: position,
                    }
                })
                .collect(),
        )
    }

    pub(super) fn infos(&self) -> Vec<MonitorInfo> {
        self.0.iter().map(|display| display.info.clone()).collect()
    }

    /// The pointer in logical pixels of the display it is on; `None` when the platform does not
    /// tell or the pointer is on no display.
    pub(super) fn cursor(&self) -> Option<Point> {
        let at = platform::cursor_position()?;
        self.0.iter().find_map(|display| {
            let (x, y) = (
                i64::from(at.x) - i64::from(display.origin.x),
                i64::from(at.y) - i64::from(display.origin.y),
            );
            let scale = display.info.scale_factor;
            let point = Point {
                x: display.info.bounds.x + x as f64 / scale,
                y: display.info.bounds.y + y as f64 / scale,
            };
            display.info.bounds.contains(point).then_some(point)
        })
    }

    /// Index of the display whose physical top-left is `origin` (how a window finds its display).
    pub(super) fn at_origin(&self, origin: PhysicalPosition<i32>) -> Option<usize> {
        self.0.iter().position(|display| display.origin == origin)
    }

    pub(super) fn scale(&self, index: Option<usize>) -> f64 {
        index
            .and_then(|index| self.0.get(index))
            .map_or(1.0, |display| display.info.scale_factor)
    }

    /// Physical top-left of a logical point: the display that contains it, else `fallback`.
    pub(super) fn physical_position(
        &self,
        point: Point,
        fallback: Option<usize>,
    ) -> Option<PhysicalPosition<i32>> {
        let display = self
            .0
            .iter()
            .find(|display| display.info.bounds.contains(point))
            .or_else(|| fallback.and_then(|index| self.0.get(index)))?;
        let scale = display.info.scale_factor;
        let offset = |value: f64, origin: f64| ((value - origin) * scale).round() as i32;
        Some(PhysicalPosition::new(
            display.origin.x + offset(point.x, display.info.bounds.x),
            display.origin.y + offset(point.y, display.info.bounds.y),
        ))
    }
}

/// Physical size of a logical one on a display with `scale`.
pub(super) fn physical_size(size: (f64, f64), scale: f64) -> PhysicalSize<u32> {
    let round = |value: f64| (value * scale).round().clamp(1.0, f64::from(u32::MAX)) as u32;
    PhysicalSize::new(round(size.0), round(size.1))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn display(x: i32, scale: f64, logical_width: f64) -> Display {
        Display {
            info: MonitorInfo {
                name: None,
                bounds: Rect {
                    x: f64::from(x) / scale,
                    y: 0.0,
                    width: logical_width,
                    height: 600.0,
                },
                work_area: Rect {
                    x: f64::from(x) / scale,
                    y: 0.0,
                    width: logical_width,
                    height: 560.0,
                },
                scale_factor: scale,
                primary: x == 0,
            },
            origin: PhysicalPosition::new(x, 0),
        }
    }

    /// 1000x600 at scale 1, and to its right (physical x = 1000) 800x600 logical at scale 2.
    fn two() -> Displays {
        Displays(vec![display(0, 1.0, 1000.0), display(1000, 2.0, 800.0)])
    }

    #[test]
    fn a_logical_point_goes_back_to_the_physical_pixels_of_its_display() {
        let displays = two();
        let at = |x: f64, y: f64| displays.physical_position(Point { x, y }, None);
        assert_eq!(at(10.0, 20.0), Some(PhysicalPosition::new(10, 20)));
        // Logical x = 1000 / 2 = 500 is where the second display starts in its own scale, but the
        // first display still covers it: the first one that contains the point wins.
        assert_eq!(at(500.0, 0.0), Some(PhysicalPosition::new(500, 0)));
        assert_eq!(at(1200.0, 0.0), Some(PhysicalPosition::new(1000 + 1400, 0)));
        assert_eq!(at(1200.0, 10.0), Some(PhysicalPosition::new(2400, 20)));
        assert_eq!(
            at(9000.0, 0.0),
            None,
            "outside every display and no fallback"
        );
        assert_eq!(
            displays.physical_position(Point { x: 9000.0, y: 0.0 }, Some(1)),
            Some(PhysicalPosition::new(
                1000 + (9000.0f64 - 500.0) as i32 * 2,
                0
            ))
        );
    }

    #[test]
    fn displays_are_found_by_their_origin_and_scale() {
        let displays = two();
        assert_eq!(displays.at_origin(PhysicalPosition::new(1000, 0)), Some(1));
        assert_eq!(displays.at_origin(PhysicalPosition::new(5, 0)), None);
        assert_eq!(displays.scale(Some(1)), 2.0);
        assert_eq!(displays.scale(None), 1.0);
        assert_eq!(displays.scale(Some(7)), 1.0);
        let infos = displays.infos();
        assert_eq!(infos.len(), 2);
        assert!(infos[0].primary && !infos[1].primary);
    }

    #[test]
    fn logical_sizes_scale_to_whole_physical_pixels_and_never_vanish() {
        assert_eq!(
            physical_size((800.0, 600.0), 1.5),
            PhysicalSize::new(1200, 900)
        );
        assert_eq!(
            physical_size((100.5, 10.0), 1.0),
            PhysicalSize::new(101, 10)
        );
        assert_eq!(physical_size((0.1, 0.0), 1.0), PhysicalSize::new(1, 1));
    }
}
