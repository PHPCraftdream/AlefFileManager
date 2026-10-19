// SPDX-License-Identifier: MIT OR Apache-2.0
//! Backend-neutral device failure and mode selection.
use alef_core::{AlefError, ErrorCode};
use std::sync::{
    atomic::{AtomicBool, AtomicU8, Ordering},
    Arc,
};

static ACTIVE_OWNERS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
const MAX_OWNERS: usize = 8;

/// Hard cap for native device-owner threads; held until the native stream is dropped.
pub struct OwnerLease;
impl OwnerLease {
    pub fn acquire() -> Result<Self, alef_core::AlefError> {
        let mut current = ACTIVE_OWNERS.load(Ordering::Acquire);
        loop {
            if current >= MAX_OWNERS {
                return Err(alef_core::AlefError::new(
                    alef_core::ErrorCode::Busy,
                    "media device owners are busy",
                ));
            }
            match ACTIVE_OWNERS.compare_exchange_weak(
                current,
                current + 1,
                Ordering::AcqRel,
                Ordering::Acquire,
            ) {
                Ok(_) => return Ok(Self),
                Err(next) => current = next,
            }
        }
    }
}
impl Drop for OwnerLease {
    fn drop(&mut self) {
        ACTIVE_OWNERS.fetch_sub(1, Ordering::AcqRel);
    }
}

pub struct StopGuard(Arc<AtomicBool>, Arc<std::sync::Condvar>);
impl StopGuard {
    pub fn new(stop: Arc<AtomicBool>, wake: Arc<std::sync::Condvar>) -> Self {
        Self(stop, wake)
    }
}
impl Drop for StopGuard {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
        self.1.notify_all();
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
#[repr(u8)]
pub enum DeviceFailure {
    PermissionDenied = 1,
    NotAvailable = 2,
    Internal = 3,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceKind {
    Camera,
    Microphone,
    Audio,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum DeviceErrorKind {
    PermissionDenied,
    NotAvailable,
    Internal,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum CpalFailureKind {
    PermissionDenied,
    DeviceNotAvailable,
    Other,
}
impl DeviceFailure {
    pub fn from_cpal(kind: CpalFailureKind) -> Self {
        match kind {
            CpalFailureKind::PermissionDenied => Self::PermissionDenied,
            CpalFailureKind::DeviceNotAvailable => Self::NotAvailable,
            CpalFailureKind::Other => Self::Internal,
        }
    }
    pub fn from_raw(raw: u8) -> Option<Self> {
        match raw {
            1 => Some(Self::PermissionDenied),
            2 => Some(Self::NotAvailable),
            3 => Some(Self::Internal),
            _ => None,
        }
    }
    pub fn error(self, device: DeviceKind) -> AlefError {
        let (label, code, text) = match (device, self) {
            (DeviceKind::Camera, Self::PermissionDenied) => {
                ("camera", ErrorCode::PermissionDenied, "permission denied")
            }
            (DeviceKind::Microphone, Self::PermissionDenied) => (
                "microphone",
                ErrorCode::PermissionDenied,
                "permission denied",
            ),
            (DeviceKind::Audio, Self::PermissionDenied) => {
                ("audio", ErrorCode::PermissionDenied, "permission denied")
            }
            (DeviceKind::Camera, Self::NotAvailable) => {
                ("camera", ErrorCode::NotAvailable, "is not available")
            }
            (DeviceKind::Microphone, Self::NotAvailable) => {
                ("microphone", ErrorCode::NotAvailable, "is not available")
            }
            (DeviceKind::Audio, Self::NotAvailable) => {
                ("audio", ErrorCode::NotAvailable, "is not available")
            }
            (DeviceKind::Camera, Self::Internal) => {
                ("camera", ErrorCode::Internal, "device failed")
            }
            (DeviceKind::Microphone, Self::Internal) => {
                ("microphone", ErrorCode::Internal, "device failed")
            }
            (DeviceKind::Audio, Self::Internal) => ("audio", ErrorCode::Internal, "device failed"),
        };
        AlefError::new(code, format!("{label} {text}"))
    }
    pub fn kind(self) -> DeviceErrorKind {
        match self {
            Self::PermissionDenied => DeviceErrorKind::PermissionDenied,
            Self::NotAvailable => DeviceErrorKind::NotAvailable,
            Self::Internal => DeviceErrorKind::Internal,
        }
    }
}
#[derive(Debug, Default)]
pub struct Failure(AtomicU8);
impl Failure {
    pub fn set(&self, failure: DeviceFailure) -> bool {
        self.0
            .compare_exchange(0, failure as u8, Ordering::AcqRel, Ordering::Acquire)
            .is_ok()
    }
    pub fn get(&self) -> Option<DeviceFailure> {
        DeviceFailure::from_raw(self.0.load(Ordering::Acquire))
    }
    pub fn error(&self, device: DeviceKind) -> Option<AlefError> {
        self.get().map(|failure| failure.error(device))
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum PixelFormat {
    Rgb,
    Bgr,
    Gray,
    Yuyv,
    Nv12,
}
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Mode {
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub format: PixelFormat,
}
/// Nearest squared width/height distance, then frame rate, then conversion cost.
pub fn select_mode(modes: &[Mode], width: u32, height: u32, fps: f64) -> Option<Mode> {
    modes
        .iter()
        .copied()
        .filter(|mode| {
            mode.width > 0
                && mode.height > 0
                && mode.width <= 16384
                && mode.height <= 16384
                && mode.fps.is_finite()
                && mode.fps > 0.0
        })
        .min_by(|a, b| {
            let distance = |mode: &Mode| {
                let dx = i128::from(mode.width) - i128::from(width);
                let dy = i128::from(mode.height) - i128::from(height);
                (dx * dx + dy * dy) as u128
            };
            distance(a)
                .cmp(&distance(b))
                .then_with(|| (a.fps - fps).abs().total_cmp(&(b.fps - fps).abs()))
                .then_with(|| rank(a.format).cmp(&rank(b.format)))
        })
}
fn rank(format: PixelFormat) -> u8 {
    match format {
        PixelFormat::Rgb => 0,
        PixelFormat::Bgr => 1,
        PixelFormat::Gray => 2,
        PixelFormat::Yuyv => 3,
        PixelFormat::Nv12 => 4,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn failures_map_to_fixed_device_messages_and_codes() {
        for (failure, code, msg) in [
            (
                DeviceFailure::PermissionDenied,
                ErrorCode::PermissionDenied,
                "camera permission denied",
            ),
            (
                DeviceFailure::NotAvailable,
                ErrorCode::NotAvailable,
                "camera is not available",
            ),
            (
                DeviceFailure::Internal,
                ErrorCode::Internal,
                "camera device failed",
            ),
        ] {
            let error = failure.error(DeviceKind::Camera);
            assert_eq!((error.code, error.message.as_str()), (code, msg));
        }
        assert_eq!(
            DeviceFailure::NotAvailable
                .error(DeviceKind::Microphone)
                .message,
            "microphone is not available"
        );
        assert_eq!(
            DeviceFailure::Internal.error(DeviceKind::Audio).message,
            "audio device failed"
        );
        assert_eq!(
            DeviceFailure::PermissionDenied.kind(),
            DeviceErrorKind::PermissionDenied
        );
    }
    #[test]
    fn errors_cover_each_device_and_raw_failure_is_first_writer_wins() {
        for (failure, code, suffix) in [
            (
                DeviceFailure::PermissionDenied,
                ErrorCode::PermissionDenied,
                "permission denied",
            ),
            (
                DeviceFailure::NotAvailable,
                ErrorCode::NotAvailable,
                "is not available",
            ),
            (
                DeviceFailure::Internal,
                ErrorCode::Internal,
                "device failed",
            ),
        ] {
            for (kind, label) in [
                (DeviceKind::Camera, "camera"),
                (DeviceKind::Microphone, "microphone"),
                (DeviceKind::Audio, "audio"),
            ] {
                let error = failure.error(kind);
                assert_eq!(error.code, code);
                assert_eq!(error.message, format!("{label} {suffix}"));
            }
        }
        let failure = Failure::default();
        assert!(failure.set(DeviceFailure::NotAvailable));
        assert!(!failure.set(DeviceFailure::Internal));
        assert_eq!(failure.get(), Some(DeviceFailure::NotAvailable));
    }

    #[test]
    fn selection_rejects_malformed_modes_and_fps_distance_beats_format_rank() {
        let malformed = [
            mode(0, 10, 30.0, PixelFormat::Rgb),
            mode(10, 10, 0.0, PixelFormat::Rgb),
            mode(10, 10, f64::NAN, PixelFormat::Rgb),
            mode(16385, 10, 30.0, PixelFormat::Rgb),
        ];
        assert_eq!(select_mode(&malformed, 10, 10, 30.0), None);
        let modes = [
            mode(100, 100, 30.0, PixelFormat::Rgb),
            mode(100, 100, 60.0, PixelFormat::Nv12),
        ];
        assert_eq!(select_mode(&modes, 100, 100, 59.0), Some(modes[1]));
    }

    #[test]
    fn cpal_failure_mapping_is_stable_for_all_error_categories() {
        assert_eq!(
            DeviceFailure::from_cpal(CpalFailureKind::PermissionDenied),
            DeviceFailure::PermissionDenied
        );
        assert_eq!(
            DeviceFailure::from_cpal(CpalFailureKind::DeviceNotAvailable),
            DeviceFailure::NotAvailable
        );
        assert_eq!(
            DeviceFailure::from_cpal(CpalFailureKind::Other),
            DeviceFailure::Internal
        );
    }
    #[test]
    fn failure_is_persistent_and_first_writer_wins() {
        let failure = Failure::default();
        assert!(failure.set(DeviceFailure::PermissionDenied));
        assert!(!failure.set(DeviceFailure::Internal));
        assert_eq!(failure.get(), Some(DeviceFailure::PermissionDenied));
    }
    fn mode(width: u32, height: u32, fps: f64, format: PixelFormat) -> Mode {
        Mode {
            width,
            height,
            fps,
            format,
        }
    }
    #[test]
    fn selection_uses_squared_geometry_distance_before_fps() {
        let modes = [
            mode(100, 100, 1.0, PixelFormat::Rgb),
            mode(101, 100, 100.0, PixelFormat::Rgb),
        ];
        assert_eq!(select_mode(&modes, 100, 100, 100.0), Some(modes[0]));
    }
    #[test]
    fn selection_uses_fps_distance_after_geometry() {
        let modes = [
            mode(100, 100, 30.0, PixelFormat::Rgb),
            mode(100, 100, 60.0, PixelFormat::Bgr),
        ];
        assert_eq!(select_mode(&modes, 100, 100, 59.0), Some(modes[1]));
    }
    #[test]
    fn selection_uses_conversion_rank_after_geometry_and_fps() {
        let modes = [
            mode(100, 100, 30.0, PixelFormat::Nv12),
            mode(100, 100, 30.0, PixelFormat::Gray),
            mode(100, 100, 30.0, PixelFormat::Rgb),
        ];
        assert_eq!(select_mode(&modes, 100, 100, 30.0), Some(modes[2]));
    }
}
