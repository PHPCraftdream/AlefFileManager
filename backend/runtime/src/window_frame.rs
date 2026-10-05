// SPDX-License-Identifier: GPL-3.0-or-later
use servo::DevicePoint;
use winit::dpi::PhysicalSize;
use winit::window::ResizeDirection;

use crate::ResizeEdge;

impl From<ResizeEdge> for ResizeDirection {
    fn from(edge: ResizeEdge) -> Self {
        match edge {
            ResizeEdge::North => Self::North,
            ResizeEdge::NorthEast => Self::NorthEast,
            ResizeEdge::East => Self::East,
            ResizeEdge::SouthEast => Self::SouthEast,
            ResizeEdge::South => Self::South,
            ResizeEdge::SouthWest => Self::SouthWest,
            ResizeEdge::West => Self::West,
            ResizeEdge::NorthWest => Self::NorthWest,
        }
    }
}

pub(crate) fn resize_hit(
    point: DevicePoint,
    size: PhysicalSize<u32>,
    scale: f64,
) -> Option<ResizeDirection> {
    let x = f64::from(point.x);
    let y = f64::from(point.y);
    let width = f64::from(size.width);
    let height = f64::from(size.height);
    if x < 0.0 || y < 0.0 || x >= width || y >= height {
        return None;
    }
    let border = 8.0 * scale;
    let left = x < border;
    let right = x >= width - border;
    let top = y < border;
    let bottom = y >= height - border;
    match (left, right, top, bottom) {
        (true, _, true, _) => Some(ResizeDirection::NorthWest),
        (_, true, true, _) => Some(ResizeDirection::NorthEast),
        (true, _, _, true) => Some(ResizeDirection::SouthWest),
        (_, true, _, true) => Some(ResizeDirection::SouthEast),
        (true, _, _, _) => Some(ResizeDirection::West),
        (_, true, _, _) => Some(ResizeDirection::East),
        (_, _, true, _) => Some(ResizeDirection::North),
        (_, _, _, true) => Some(ResizeDirection::South),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn corners_take_priority_over_edges_and_dpi_is_respected() {
        let size = PhysicalSize::new(1200, 800);
        assert_eq!(
            resize_hit(DevicePoint::new(4.0, 4.0), size, 1.0),
            Some(ResizeDirection::NorthWest)
        );
        assert_eq!(
            resize_hit(DevicePoint::new(1194.0, 400.0), size, 1.0),
            Some(ResizeDirection::East)
        );
        assert_eq!(resize_hit(DevicePoint::new(12.0, 400.0), size, 1.0), None);
        assert_eq!(
            resize_hit(DevicePoint::new(12.0, 400.0), size, 2.0),
            Some(ResizeDirection::West)
        );
    }

    #[test]
    fn outside_and_zero_sized_windows_do_not_start_resize() {
        assert_eq!(
            resize_hit(
                DevicePoint::new(-1.0, 400.0),
                PhysicalSize::new(1200, 800),
                1.0
            ),
            None
        );
        assert_eq!(
            resize_hit(
                DevicePoint::new(1200.0, 400.0),
                PhysicalSize::new(1200, 800),
                1.0
            ),
            None
        );
        assert_eq!(
            resize_hit(DevicePoint::new(0.0, 0.0), PhysicalSize::new(0, 0), 1.0),
            None
        );
    }
}
