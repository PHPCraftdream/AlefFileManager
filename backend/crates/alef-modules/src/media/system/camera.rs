// SPDX-License-Identifier: MIT OR Apache-2.0
//! System cameras through nokhwa. One process-wide service thread owns every native camera and
//! performs enumeration, open, capture, and drop. At most one stream is active. Native frame reads
//! may block indefinitely, so clients signal stop and never join the service thread.
use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc, Condvar, Mutex, OnceLock,
    },
    time::Duration,
};

use alef_core::{AlefError, ErrorCode};
use bytes::Bytes;
use nokhwa::utils::{
    ApiBackend, CameraFormat, CameraIndex, FrameFormat, RequestedFormat, RequestedFormatType,
};
use nokhwa::{Camera, NokhwaError};

use super::convert::{
    rgba_from_bgr, rgba_from_gray, rgba_from_nv12, rgba_from_rgb, rgba_from_yuyv,
};
use crate::media::{
    device::queue::{unix_ms, CameraFrame},
    device::{
        backend::{CameraBackend, CameraInfo, DeviceStream},
        state::{select_mode, Mode, PixelFormat},
    },
    invalid,
};

const REQUEST_WIDTH: u32 = 1280;
const REQUEST_HEIGHT: u32 = 720;
const ID_PREFIX: &str = "system-";
const OPEN_TIMEOUT: Duration = Duration::from_secs(60);
const FRAME_TIMEOUT: Duration = Duration::from_secs(1);
const SERVICE_QUEUE: usize = 8;
const CONVERTIBLE: [FrameFormat; 5] = [
    FrameFormat::RAWRGB,
    FrameFormat::RAWBGR,
    FrameFormat::YUYV,
    FrameFormat::NV12,
    FrameFormat::GRAY,
];
static CAMERA_BUSY: AtomicBool = AtomicBool::new(false);
static CAMERA_SERVICE: OnceLock<Result<CameraService, AlefError>> = OnceLock::new();

type OpenResult = Result<(u32, u32, f64), AlefError>;
enum Request {
    List(mpsc::SyncSender<Result<Vec<CameraInfo>, AlefError>>),
    Open {
        index: u32,
        id: String,
        fps: f64,
        shared: Arc<Shared>,
        reply: mpsc::SyncSender<OpenResult>,
        lease: CameraLease,
    },
}
struct CameraService {
    sender: mpsc::SyncSender<Request>,
}
fn service() -> Result<&'static CameraService, AlefError> {
    match CAMERA_SERVICE.get_or_init(|| {
        let (sender, receiver) = mpsc::sync_channel(SERVICE_QUEUE);
        std::thread::Builder::new()
            .name("alef-camera-service".into())
            .spawn(move || service_loop(receiver))
            .map_err(|_| {
                AlefError::new(ErrorCode::Internal, "the camera service could not start")
            })?;
        Ok(CameraService { sender })
    }) {
        Ok(service) => Ok(service),
        Err(error) => Err(error.clone()),
    }
}

#[derive(Debug, Default)]
pub struct SystemCamera;
impl CameraBackend for SystemCamera {
    fn list(&self) -> Result<Vec<CameraInfo>, AlefError> {
        if CAMERA_BUSY.load(Ordering::Acquire) {
            return Err(busy());
        }
        let (reply, receipt) = mpsc::sync_channel(1);
        service()?
            .sender
            .try_send(Request::List(reply))
            .map_err(|_| busy())?;
        receipt
            .recv_timeout(OPEN_TIMEOUT)
            .map_err(|_| service_timeout())?
    }

    fn open(&self, id: &str, fps: f64) -> Result<Arc<DeviceStream<CameraFrame>>, AlefError> {
        let index = parse_id(id)?;
        if !fps.is_finite() || !(1.0..=120.0).contains(&fps) {
            return Err(invalid("fps must be a number between 1 and 120".to_owned()));
        }
        let lease = CameraLease::acquire()?;
        let service = match service() {
            Ok(service) => service,
            Err(error) => {
                drop(lease);
                return Err(error);
            }
        };
        let shared = Arc::new(Shared::new());
        let (reply, receipt) = mpsc::sync_channel(1);
        let request = Request::Open {
            index,
            id: id.to_owned(),
            fps,
            shared: shared.clone(),
            reply,
            lease,
        };
        if service.sender.try_send(request).is_err() {
            drop(shared);
            return Err(busy());
        }
        let (width, height, actual_fps) = receipt.recv_timeout(OPEN_TIMEOUT).map_err(|_| {
            shared.stop();
            service_timeout()
        })??;
        let interval = Duration::from_secs_f64(1.0 / actual_fps.max(1.0));
        let wait = FRAME_TIMEOUT.max(interval.saturating_mul(3));
        let consumer = shared.clone();
        let cleanup = shared.clone();
        DeviceStream::start(
            crate::media::virtuals::FRAME_QUEUE,
            interval,
            move || cleanup.stop(),
            move |seq, queue| match consumer.pop(wait) {
                Pop::Frame(slot) => CameraFrame {
                    width: slot.width,
                    height: slot.height,
                    seq,
                    timestamp_ms: slot.captured_ms,
                    dropped: queue.drops(),
                    payload: Bytes::from(slot.payload),
                },
                Pop::Stopped => {
                    queue.close();
                    terminal_frame(width, height, seq, queue.drops())
                }
                Pop::Failed(error) => {
                    queue.fail(error);
                    terminal_frame(width, height, seq, queue.drops())
                }
                Pop::TimedOut => {
                    eprintln!("ALEF_MEDIA_CAMERA_STUCK");
                    consumer.stop();
                    queue.fail(AlefError::new(
                        ErrorCode::NotAvailable,
                        "camera frame timed out",
                    ));
                    terminal_frame(width, height, seq, queue.drops())
                }
            },
        )
    }
}

struct CameraLease;
impl CameraLease {
    fn acquire() -> Result<Self, AlefError> {
        CAMERA_BUSY
            .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
            .map(|_| Self)
            .map_err(|_| busy())
    }
}
impl Drop for CameraLease {
    fn drop(&mut self) {
        CAMERA_BUSY.store(false, Ordering::Release);
    }
}
fn busy() -> AlefError {
    AlefError::new(ErrorCode::Busy, "the camera service is busy")
}
fn service_timeout() -> AlefError {
    AlefError::new(ErrorCode::Timeout, "the camera operation timed out")
}

fn service_loop(receiver: mpsc::Receiver<Request>) {
    while let Ok(request) = receiver.recv() {
        match request {
            Request::List(reply) => {
                let _ = reply.send(list_native());
            }
            Request::Open {
                index,
                id,
                fps,
                shared,
                reply,
                lease,
            } => {
                #[cfg(target_os = "macos")]
                if !shared.is_stopped() {
                    if let Err(error) = ensure_camera_permission() {
                        shared.fail(error.clone());
                        let _ = reply.send(Err(error));
                        drop(lease);
                        continue;
                    }
                }
                if shared.is_stopped() {
                    drop(lease);
                    continue;
                }
                let result = open_on_service(index, &id, fps);
                match result {
                    Ok((mut camera, _format)) => {
                        let actual = camera.camera_format();
                        if actual.frame_rate() == 0 {
                            shared.fail(open_unavailable());
                            let _ = reply.send(Err(open_unavailable()));
                            drop(camera);
                            drop(lease);
                            continue;
                        }
                        let opened = (
                            actual.width(),
                            actual.height(),
                            f64::from(actual.frame_rate()),
                        );
                        if reply.send(Ok(opened)).is_ok() && !shared.is_stopped() {
                            capture(&mut camera, &shared);
                        }
                        drop(camera);
                    }
                    Err(error) => {
                        shared.fail(error.clone());
                        let _ = reply.send(Err(error));
                    }
                }
                drop(lease);
            }
        }
    }
}
#[cfg(target_os = "macos")]
fn ensure_camera_permission() -> Result<(), AlefError> {
    if nokhwa::nokhwa_check() {
        return Ok(());
    }
    let (sender, receiver) = mpsc::sync_channel(1);
    nokhwa::nokhwa_initialize(move |granted| {
        let _ = sender.send(granted);
    });
    match receiver.recv_timeout(OPEN_TIMEOUT) {
        Ok(true) => Ok(()),
        Ok(false) => Err(open_denied()),
        Err(_) => Err(service_timeout()),
    }
}

fn list_native() -> Result<Vec<CameraInfo>, AlefError> {
    nokhwa::query(ApiBackend::Auto)
        .map(|items| {
            items
                .iter()
                .map(|item| CameraInfo {
                    id: camera_id(item.index()),
                    label: camera_label(item),
                    // Device modes are not queried during list; negotiated open frames carry size/rate.
                    width: 0,
                    height: 0,
                    fps: 0.0,
                })
                .collect()
        })
        .map_err(|_| list_error())
}

struct Slot {
    width: u32,
    height: u32,
    payload: Vec<u8>,
    captured_ms: u64,
}
enum Pop {
    Frame(Slot),
    Stopped,
    Failed(AlefError),
    TimedOut,
}
struct Shared {
    slots: Mutex<VecDeque<Slot>>,
    available: Condvar,
    closed: AtomicBool,
    stop: AtomicBool,
    failure: Mutex<Option<AlefError>>,
}
impl Shared {
    fn new() -> Self {
        Self {
            slots: Mutex::new(VecDeque::new()),
            available: Condvar::new(),
            closed: AtomicBool::new(false),
            stop: AtomicBool::new(false),
            failure: Mutex::new(None),
        }
    }
    fn is_stopped(&self) -> bool {
        self.stop.load(Ordering::Acquire)
    }
    fn stop(&self) {
        self.stop.store(true, Ordering::Release);
        self.available.notify_all();
    }
    fn push(&self, slot: Slot) {
        let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        if slots.len() >= crate::media::virtuals::FRAME_QUEUE {
            slots.pop_front();
        }
        slots.push_back(slot);
        drop(slots);
        self.available.notify_one();
    }
    fn fail(&self, error: AlefError) {
        *self.failure.lock().unwrap_or_else(|e| e.into_inner()) = Some(error);
        self.closed.store(true, Ordering::Release);
        self.available.notify_all();
    }
    fn pop(&self, wait: Duration) -> Pop {
        let deadline = std::time::Instant::now() + wait;
        let mut slots = self.slots.lock().unwrap_or_else(|e| e.into_inner());
        loop {
            if let Some(slot) = slots.pop_front() {
                return Pop::Frame(slot);
            }
            if let Some(error) = self
                .failure
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
            {
                return Pop::Failed(error);
            }
            if self.stop.load(Ordering::Acquire) {
                return Pop::Stopped;
            }
            if self.closed.load(Ordering::Acquire) {
                return Pop::Stopped;
            }
            let now = std::time::Instant::now();
            if now >= deadline {
                return Pop::TimedOut;
            }
            let (guard, timeout) = self
                .available
                .wait_timeout(slots, deadline.saturating_duration_since(now))
                .unwrap_or_else(|e| e.into_inner());
            slots = guard;
            if timeout.timed_out() {
                return Pop::TimedOut;
            }
        }
    }
}
fn capture(camera: &mut Camera, shared: &Shared) {
    // Native frame calls pace capture; do not sleep after a successful read.
    while !shared.is_stopped() {
        match camera.frame() {
            Ok(frame) => {
                let width = frame.resolution().width();
                let height = frame.resolution().height();
                match rgba(frame.source_frame_format(), frame.buffer(), width, height) {
                    Some(payload) => shared.push(Slot {
                        width,
                        height,
                        payload,
                        captured_ms: unix_ms(),
                    }),
                    None => {
                        shared.fail(open_unavailable());
                        break;
                    }
                }
            }
            Err(_) => {
                shared.fail(AlefError::new(
                    ErrorCode::NotAvailable,
                    "camera capture failed",
                ));
                break;
            }
        }
    }
}

fn open_on_service(index: u32, id: &str, fps: f64) -> Result<(Camera, CameraFormat), AlefError> {
    let listed = nokhwa::query(ApiBackend::Auto).map_err(|_| list_error())?;
    let index = listed
        .iter()
        .find(|info| matches!(info.index(), CameraIndex::Index(found) if *found == index))
        .map(|info| info.index().clone())
        .ok_or_else(|| AlefError::new(ErrorCode::NotFound, format!("unknown camera {id}")))?;
    let mut camera = Camera::new(
        index,
        RequestedFormat::with_formats(RequestedFormatType::None, &CONVERTIBLE),
    )
    .map_err(|error| match error {
        NokhwaError::InitializeError { .. } => no_decodable(),
        _ => open_unavailable(),
    })?;
    let compatible = camera
        .compatible_camera_formats()
        .map_err(|_| open_unavailable())?;
    let modes: Vec<Mode> = compatible
        .iter()
        .filter_map(|format| {
            let pixel = match format.format() {
                FrameFormat::RAWRGB => PixelFormat::Rgb,
                FrameFormat::RAWBGR => PixelFormat::Bgr,
                FrameFormat::GRAY => PixelFormat::Gray,
                FrameFormat::YUYV => PixelFormat::Yuyv,
                FrameFormat::NV12 => PixelFormat::Nv12,
                _ => return None,
            };
            Some(Mode {
                width: format.width(),
                height: format.height(),
                fps: f64::from(format.frame_rate()),
                format: pixel,
            })
        })
        .collect();
    let mode = select_mode(&modes, REQUEST_WIDTH, REQUEST_HEIGHT, fps).ok_or_else(no_decodable)?;
    let format = compatible
        .iter()
        .find(|format| {
            format.width() == mode.width
                && format.height() == mode.height
                && f64::from(format.frame_rate()) == mode.fps
                && frame_pixel(format.format()) == Some(mode.format)
        })
        .copied()
        .ok_or_else(no_decodable)?;
    camera
        .set_camera_requset(RequestedFormat::with_formats(
            RequestedFormatType::Exact(format),
            &CONVERTIBLE,
        ))
        .map_err(|_| open_unavailable())?;
    camera.open_stream().map_err(|_| open_unavailable())?;
    if !CONVERTIBLE.contains(&camera.frame_format()) {
        return Err(no_decodable());
    }
    let actual = camera.camera_format();
    Ok((camera, actual))
}
fn frame_pixel(format: FrameFormat) -> Option<PixelFormat> {
    Some(match format {
        FrameFormat::RAWRGB => PixelFormat::Rgb,
        FrameFormat::RAWBGR => PixelFormat::Bgr,
        FrameFormat::GRAY => PixelFormat::Gray,
        FrameFormat::YUYV => PixelFormat::Yuyv,
        FrameFormat::NV12 => PixelFormat::Nv12,
        _ => return None,
    })
}
fn rgba(format: FrameFormat, data: &[u8], width: u32, height: u32) -> Option<Vec<u8>> {
    match format {
        FrameFormat::RAWRGB => rgba_from_rgb(data, width, height),
        FrameFormat::RAWBGR => rgba_from_bgr(data, width, height),
        FrameFormat::YUYV => rgba_from_yuyv(data, width, height),
        FrameFormat::NV12 => rgba_from_nv12(data, width, height),
        FrameFormat::GRAY => rgba_from_gray(data, width, height),
        _ => None,
    }
}
fn terminal_frame(width: u32, height: u32, seq: u64, dropped: u64) -> CameraFrame {
    CameraFrame {
        width,
        height,
        seq,
        timestamp_ms: unix_ms(),
        dropped,
        payload: Bytes::from_static(&[0, 0, 0, 255]),
    }
}
fn parse_id(id: &str) -> Result<u32, AlefError> {
    id.strip_prefix(ID_PREFIX)
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| AlefError::new(ErrorCode::NotFound, format!("unknown camera {id}")))
}
fn camera_id(index: &CameraIndex) -> String {
    match index {
        CameraIndex::Index(index) => format!("{ID_PREFIX}{index}"),
        CameraIndex::String(index) => format!("{ID_PREFIX}{index}"),
    }
}
fn camera_label(info: &nokhwa::utils::CameraInfo) -> String {
    let human = info.human_name();
    if human.is_empty() {
        info.description().to_owned()
    } else {
        human
    }
}
#[cfg(target_os = "macos")]
fn open_denied() -> AlefError {
    AlefError::new(
        ErrorCode::PermissionDenied,
        "the operating system denied the camera",
    )
}
fn open_unavailable() -> AlefError {
    AlefError::new(ErrorCode::NotAvailable, "the camera is not available")
}
fn no_decodable() -> AlefError {
    AlefError::new(
        ErrorCode::NotAvailable,
        "the camera offers no decodable frame format",
    )
}
fn list_error() -> AlefError {
    AlefError::new(ErrorCode::Internal, "the cameras could not be listed")
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn fixed_error_mappings_are_stable() {
        #[cfg(target_os = "macos")]
        assert_eq!(open_denied().code, ErrorCode::PermissionDenied);
        assert_eq!(open_unavailable().code, ErrorCode::NotAvailable);
        assert_eq!(no_decodable().code, ErrorCode::NotAvailable);
        assert_eq!(list_error().code, ErrorCode::Internal);
    }
    #[test]
    fn a_failed_camera_delivers_its_buffered_frames_and_then_the_error() {
        let shared = Shared::new();
        shared.push(Slot {
            width: 1,
            height: 1,
            payload: vec![0; 4],
            captured_ms: 0,
        });
        let error = open_unavailable();
        shared.fail(error.clone());
        assert!(matches!(shared.pop(Duration::ZERO), Pop::Frame(_)));
        assert!(matches!(shared.pop(Duration::ZERO), Pop::Failed(found) if found == error));
    }

    #[test]
    fn native_pixel_formats_map_only_supported_formats() {
        assert_eq!(frame_pixel(FrameFormat::RAWRGB), Some(PixelFormat::Rgb));
        assert_eq!(frame_pixel(FrameFormat::MJPEG), None);
    }
}
