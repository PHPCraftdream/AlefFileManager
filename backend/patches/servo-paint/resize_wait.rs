/* This Source Code Form is subject to the terms of the Mozilla Public
 * License, v. 2.0. If a copy of the MPL was not distributed with this
 * file, You can obtain one at https://mozilla.org/MPL/2.0/. */

//! Tracks a resize that is waiting for content laid out at the new viewport size.
//!
//! While a [`ResizeWait`] is active on the [`Painter`](crate::painter::Painter), `Resize` and
//! `NewWebRenderFrame` repaint reasons are suppressed so the embedder is not told a frame is
//! ready until WebRender presents a frame built from a display list at the new surface size.

use std::time::{Duration, Instant};

use euclid::{Scale, Size2D};
use style_traits::CSSPixel;
use webrender_api::units::DevicePixel;

/// Safety valve: never wait longer than this for a reflow that may never come.
const MAX_WAIT: Duration = Duration::from_millis(250);
/// Device-pixel tolerance when comparing the layout viewport size to the surface size.
const SIZE_TOLERANCE: f32 = 1.0;

/// State for a resize whose new-size content has not been presented yet.
pub(crate) struct ResizeWait {
    /// The device-pixel surface size the content must be laid out for.
    pub(crate) target_size: Size2D<u32, DevicePixel>,
    pub(crate) started_at: Instant,
    /// True once the root pipeline display list matching `target_size` has been sent.
    pub(crate) matching_dl_sent: bool,
    /// Frames requested via `generate_frame` before the matching display list transaction was
    /// sent. WebRender processes transactions in order, so this many `NewWebRenderFrameReady`
    /// notifications must be ignored before a frame can be trusted to include the new content.
    pub(crate) stale_frames: usize,
}

/// Whether a display list laid out at `viewport_size` * `hidpi_scale_factor` is laid out for
/// `target_size` device pixels (within [`SIZE_TOLERANCE`]).
pub(crate) fn device_size_matches(
    viewport_size: Size2D<f32, CSSPixel>,
    hidpi_scale_factor: Scale<f32, CSSPixel, DevicePixel>,
    target_size: Size2D<u32, DevicePixel>,
) -> bool {
    let device_size = viewport_size * hidpi_scale_factor.get();
    (device_size.width - target_size.width as f32).abs() <= SIZE_TOLERANCE &&
        (device_size.height - target_size.height as f32).abs() <= SIZE_TOLERANCE
}

impl ResizeWait {
    pub(crate) fn new(target_size: Size2D<u32, DevicePixel>, stale_frames: usize) -> Self {
        Self {
            target_size,
            started_at: Instant::now(),
            matching_dl_sent: false,
            stale_frames,
        }
    }

    pub(crate) fn expired(&self) -> bool {
        self.started_at.elapsed() > MAX_WAIT
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn size(w: f32, h: f32) -> Size2D<f32, CSSPixel> {
        Size2D::new(w, h)
    }

    fn target(w: u32, h: u32) -> Size2D<u32, DevicePixel> {
        Size2D::new(w, h)
    }

    #[test]
    fn exact_match_at_hidpi_one() {
        assert!(device_size_matches(size(800., 600.), Scale::new(1.), target(800, 600)));
    }

    #[test]
    fn exact_match_at_hidpi_two() {
        assert!(device_size_matches(size(400., 300.), Scale::new(2.), target(800, 600)));
    }

    #[test]
    fn fractional_viewport_rounds_within_tolerance() {
        assert!(device_size_matches(size(400.5, 300.25), Scale::new(2.), target(801, 601)));
        assert!(device_size_matches(size(399.75, 299.5), Scale::new(2.), target(799, 599)));
    }

    #[test]
    fn mismatch_rejected() {
        assert!(!device_size_matches(size(800., 600.), Scale::new(1.), target(803, 600)));
        assert!(!device_size_matches(size(800., 600.), Scale::new(1.), target(800, 598)));
        assert!(!device_size_matches(size(800., 600.), Scale::new(2.), target(800, 600)));
    }

    #[test]
    fn stale_frame_gate_requires_matching_dl_then_one_fresh_ready() {
        // Simulates: 2 frames in flight, matching DL sent, then readies arrive.
        let mut wait = ResizeWait::new(target(800, 600), 2);
        assert!(!wait.matching_dl_sent);
        wait.matching_dl_sent = true;
        // First two readies are for pre-DL frames.
        wait.stale_frames -= 1;
        assert!(wait.stale_frames > 0);
        wait.stale_frames -= 1;
        assert_eq!(wait.stale_frames, 0);
        // Without a matching DL the gate never opens even when stale frames drain.
        let mut wait = ResizeWait::new(target(800, 600), 1);
        wait.stale_frames -= 1;
        assert!(!wait.matching_dl_sent);
    }

    #[test]
    fn expiry_is_time_based() {
        let wait = ResizeWait::new(target(800, 600), 0);
        assert!(!wait.expired());
    }
}
