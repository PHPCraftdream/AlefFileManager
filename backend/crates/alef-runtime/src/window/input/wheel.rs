// SPDX-License-Identifier: MIT OR Apache-2.0
use std::io;

use crate::window::platform::{self, ScrollAxis, DEFAULT_SCROLL_UNITS};
use servo::{WheelDelta, WheelMode};
use winit::dpi::PhysicalSize;
use winit::event::MouseScrollDelta;

// Servo 0.6's winit embedding reference uses 76 logical pixels per wheel tick.
const REFERENCE_TICK_PIXELS: f64 = 76.0;
const SCROLL_BY_PAGE: u32 = u32::MAX;

pub(crate) fn normalize(
    delta: MouseScrollDelta,
    viewport: PhysicalSize<u32>,
    scale: f64,
) -> io::Result<WheelDelta> {
    let (x, y) = match delta {
        MouseScrollDelta::PixelDelta(position) => (position.x, position.y),
        MouseScrollDelta::LineDelta(x, y) => {
            let horizontal = if x == 0.0 {
                0
            } else {
                platform::wheel_scroll_units(ScrollAxis::Horizontal)?
            };
            let vertical = if y == 0.0 {
                0
            } else {
                platform::wheel_scroll_units(ScrollAxis::Vertical)?
            };
            (
                tick_pixels(f64::from(x), horizontal, viewport.width, scale),
                tick_pixels(f64::from(y), vertical, viewport.height, scale),
            )
        }
    };
    // Servo's default scroll action consumes device pixels, regardless of delta mode.
    Ok(WheelDelta {
        x,
        y,
        z: 0.0,
        mode: WheelMode::DeltaPixel,
    })
}

fn tick_pixels(ticks: f64, units: u32, viewport: u32, scale: f64) -> f64 {
    if units == SCROLL_BY_PAGE {
        ticks * f64::from(viewport)
    } else {
        ticks * REFERENCE_TICK_PIXELS * f64::from(units) / f64::from(DEFAULT_SCROLL_UNITS) * scale
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn discrete_wheel_preserves_fractional_direction_and_dpi() {
        assert_eq!(tick_pixels(-0.25, 3, 800, 2.0), -38.0);
        assert_eq!(tick_pixels(2.0, 6, 800, 1.0), 304.0);
    }

    #[test]
    fn disabled_and_page_scroll_policies_are_not_replaced_with_defaults() {
        assert_eq!(tick_pixels(1.0, 0, 800, 1.0), 0.0);
        assert_eq!(tick_pixels(-0.5, SCROLL_BY_PAGE, 800, 2.0), -400.0);
    }
}
