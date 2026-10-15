// SPDX-License-Identifier: MIT OR Apache-2.0
//! `shell.fileIcon`: icon lookup with filesystem, encoding and OS work off async runtime threads.
#[cfg(windows)]
use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
use std::{
    collections::{HashMap, VecDeque},
    sync::{Arc, Mutex},
    time::Duration,
};

use alef_core::{
    registry::{command::Reply, context::CallContext, dispatch::Registry},
    security::{consent::Decision, permissions::Permission},
    session::executor::BlockingExecutor,
    AlefError, ErrorCode,
};
use serde::Deserialize;

#[cfg(test)]
use std::io::Cursor;

const GENERIC: &[u8] = include_bytes!("icons/generic.png");
const SIZES: &[u32] = &[16, 24, 32, 48, 64, 128, 256];

fn default_size() -> u32 {
    32
}

#[derive(Debug, Clone, Deserialize, PartialEq, Eq)]
#[serde(deny_unknown_fields)]
pub struct FileIconArgs {
    pub target: IconTarget,
    #[serde(default = "default_size")]
    pub size: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(untagged)]
pub enum IconTarget {
    Path(PathTarget),
    Extension(ExtensionTarget),
    Mime(MimeTarget),
    Kind(KindTarget),
}

#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PathTarget {
    pub path: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtensionTarget {
    pub extension: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MimeTarget {
    pub mime: String,
}
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct KindTarget {
    pub kind: IconKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum IconKind {
    Folder,
    File,
    Drive,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IconImage {
    pub width: u32,
    pub height: u32,
    pub png: Vec<u8>,
}

pub fn check_size(size: u32) -> Result<(), AlefError> {
    if SIZES.contains(&size) {
        Ok(())
    } else {
        Err(AlefError::new(
            ErrorCode::InvalidArgument,
            format!("shell.fileIcon: size must be one of {SIZES:?}"),
        ))
    }
}

pub fn nearest_size(requested: u32) -> u32 {
    *SIZES
        .iter()
        .min_by_key(|&&size| (size.abs_diff(requested), size))
        .expect("SIZES is not empty")
}

pub fn rgba_to_png(width: u32, height: u32, rgba: &[u8]) -> Result<Vec<u8>, AlefError> {
    let len = (width as usize)
        .checked_mul(height as usize)
        .and_then(|n| n.checked_mul(4));
    if len != Some(rgba.len()) {
        return Err(AlefError::new(
            ErrorCode::Internal,
            format!("shell.fileIcon: invalid RGBA buffer for {width}x{height}"),
        ));
    }
    let mut out = Vec::new();
    let mut encoder = png::Encoder::new(&mut out, width, height);
    encoder.set_color(png::ColorType::Rgba);
    encoder.set_depth(png::BitDepth::Eight);
    let mut writer = encoder.write_header().map_err(png_error)?;
    writer.write_image_data(rgba).map_err(png_error)?;
    writer.finish().map_err(png_error)?;
    Ok(out)
}

fn png_error(error: impl std::fmt::Display) -> AlefError {
    AlefError::new(ErrorCode::Internal, format!("shell.fileIcon: {error}"))
}

pub fn png_dimensions(bytes: &[u8]) -> Option<(u32, u32)> {
    const SIG: [u8; 8] = [0x89, b'P', b'N', b'G', 13, 10, 26, 10];
    if bytes.len() < 24
        || !bytes.starts_with(&SIG)
        || &bytes[12..16] != b"IHDR"
        || u32::from_be_bytes(bytes[8..12].try_into().ok()?) != 13
    {
        return None;
    }
    Some((
        u32::from_be_bytes(bytes[16..20].try_into().ok()?),
        u32::from_be_bytes(bytes[20..24].try_into().ok()?),
    ))
}

pub fn unpremultiply_bgra_to_rgba(bytes: &mut [u8]) {
    for pixel in bytes.chunks_exact_mut(4) {
        let [b, g, r, a] = [pixel[0], pixel[1], pixel[2], pixel[3]];
        let plain = |c: u8| {
            if a == 0 {
                0
            } else {
                ((c as u32 * 255 / a as u32).min(255)) as u8
            }
        };
        pixel.copy_from_slice(&[plain(r), plain(g), plain(b), a]);
    }
}

pub fn mime_to_extension(mime: &str) -> Option<&'static str> {
    let mime = mime.split(';').next()?.trim().to_ascii_lowercase();
    Some(match mime.as_str() {
        "application/pdf" => "pdf",
        "image/png" => "png",
        "image/jpeg" => "jpg",
        "image/gif" => "gif",
        "image/webp" => "webp",
        "image/svg+xml" => "svg",
        "text/plain" => "txt",
        "text/html" => "html",
        "text/css" => "css",
        "text/csv" => "csv",
        "text/markdown" => "md",
        "application/json" => "json",
        "application/xml" => "xml",
        "application/zip" => "zip",
        "application/gzip" => "gz",
        "application/x-tar" => "tar",
        "application/x-7z-compressed" => "7z",
        "application/vnd.rar" => "rar",
        "audio/mpeg" => "mp3",
        "audio/wav" => "wav",
        "audio/ogg" => "ogg",
        "audio/flac" => "flac",
        "video/mp4" => "mp4",
        "video/webm" => "webm",
        "video/x-msvideo" => "avi",
        "application/msword" => "doc",
        "application/vnd.openxmlformats-officedocument.wordprocessingml.document" => "docx",
        "application/vnd.ms-excel" => "xls",
        "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet" => "xlsx",
        "application/vnd.ms-powerpoint" => "ppt",
        "application/vnd.openxmlformats-officedocument.presentationml.presentation" => "pptx",
        _ => return None,
    })
}

pub fn extension_key(extension: &str) -> String {
    let trimmed = extension.trim();
    trimmed
        .strip_prefix('.')
        .unwrap_or(trimmed)
        .to_ascii_lowercase()
}

fn invalid(message: impl Into<String>) -> AlefError {
    AlefError::new(ErrorCode::InvalidArgument, message)
}

fn validate_target(target: IconTarget) -> Result<IconTarget, AlefError> {
    match target {
        IconTarget::Extension(mut t) => {
            t.extension = extension_key(&t.extension);
            if t.extension.is_empty()
                || t.extension.len() > 64
                || !t
                    .extension
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b"-_+".contains(&b))
            {
                return Err(invalid("shell.fileIcon: invalid extension"));
            }
            Ok(IconTarget::Extension(t))
        }
        IconTarget::Mime(mut t) => {
            t.mime = t
                .mime
                .split(';')
                .next()
                .unwrap_or("")
                .trim()
                .to_ascii_lowercase();
            let mut parts = t.mime.split('/');
            let valid_part = |s: &str| {
                !s.is_empty()
                    && s.len() <= 127
                    && s.bytes()
                        .all(|b| b.is_ascii_alphanumeric() || b"!#$&^_.+-".contains(&b))
            };
            if !valid_part(parts.next().unwrap_or(""))
                || !valid_part(parts.next().unwrap_or(""))
                || parts.next().is_some()
            {
                return Err(invalid("shell.fileIcon: invalid mime type"));
            }
            Ok(IconTarget::Mime(t))
        }
        other => Ok(other),
    }
}

pub(crate) struct IconCache {
    entries: Mutex<CacheState>,
}
#[derive(Debug, Default)]
struct CacheState {
    map: HashMap<String, IconImage>,
    order: VecDeque<String>,
    capacity: usize,
}
impl IconCache {
    pub(crate) fn new(capacity: usize) -> Self {
        Self {
            entries: Mutex::new(CacheState {
                capacity,
                ..CacheState::default()
            }),
        }
    }
    pub fn get(&self, key: &str) -> Option<IconImage> {
        let mut s = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        let image = s.map.get(key)?.clone();
        if let Some(i) = s.order.iter().position(|held| held == key) {
            s.order.remove(i);
        }
        s.order.push_back(key.to_owned());
        Some(image)
    }
    pub fn put(&self, key: String, image: IconImage) {
        let mut s = self.entries.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(i) = s.order.iter().position(|held| held == &key) {
            s.order.remove(i);
        }
        s.map.insert(key.clone(), image);
        s.order.push_back(key);
        while s.order.len() > s.capacity {
            if let Some(old) = s.order.pop_front() {
                s.map.remove(&old);
            }
        }
    }
}

pub(crate) fn generic_cache_key(target: &IconTarget, size: u32) -> Option<String> {
    Some(match target {
        IconTarget::Path(_) => return None,
        IconTarget::Extension(t) => format!("ext:{}|{size}", extension_key(&t.extension)),
        IconTarget::Mime(t) => format!("mime:{}|{size}", t.mime.to_ascii_lowercase()),
        IconTarget::Kind(t) => format!(
            "kind:{}|{size}",
            match t.kind {
                IconKind::Folder => "folder",
                IconKind::File => "file",
                IconKind::Drive => "drive",
            }
        ),
    })
}

pub(crate) fn generic_fallback() -> IconImage {
    let (width, height) = png_dimensions(GENERIC).unwrap_or((32, 32));
    IconImage {
        width,
        height,
        png: GENERIC.to_vec(),
    }
}

#[derive(Debug)]
pub(crate) struct IconJob {
    pub target: IconTarget,
    pub size: u32,
    pub reply: JobReply,
}
pub(crate) type JobReceiver = tokio::sync::mpsc::Receiver<IconJob>;
#[cfg(windows)]
pub(crate) struct WindowsJobSender {
    sender: Option<tokio::sync::mpsc::Sender<IconJob>>,
    wake: Arc<OwnedHandle>,
}
#[cfg(windows)]
impl Clone for WindowsJobSender {
    fn clone(&self) -> Self {
        Self {
            sender: Some(
                self.sender
                    .as_ref()
                    .expect("sender exists before drop")
                    .clone(),
            ),
            wake: self.wake.clone(),
        }
    }
}
#[cfg(windows)]
impl Drop for WindowsJobSender {
    fn drop(&mut self) {
        // Close this clone before signalling; final disconnect can never be lost in a wait race.
        drop(self.sender.take());
        // SAFETY: the event handle remains alive throughout Drop.
        unsafe { windows_sys::Win32::System::Threading::SetEvent(self.wake.as_raw_handle() as _) };
    }
}

#[cfg(windows)]
impl WindowsJobSender {
    pub(crate) fn try_send(
        &self,
        job: IconJob,
    ) -> Result<(), tokio::sync::mpsc::error::TrySendError<IconJob>> {
        self.sender
            .as_ref()
            .expect("sender exists before drop")
            .try_send(job)
            .map(|()| {
                // SAFETY: the event handle is owned and remains alive for this call.
                unsafe {
                    windows_sys::Win32::System::Threading::SetEvent(self.wake.as_raw_handle() as _);
                }
            })
    }
    pub(crate) fn channel() -> Result<(Self, JobReceiver, Arc<OwnedHandle>), AlefError> {
        // SAFETY: unnamed auto-reset event, initially unsignalled.
        let event = unsafe {
            windows_sys::Win32::System::Threading::CreateEventW(
                std::ptr::null(),
                0,
                0,
                std::ptr::null(),
            )
        };
        if event.is_null() {
            return Err(png_error("failed to create icon worker event"));
        }
        // SAFETY: CreateEventW returns a new owned handle.
        let wake = Arc::new(unsafe { OwnedHandle::from_raw_handle(event as _) });
        let (sender, receiver) = tokio::sync::mpsc::channel(32);
        Ok((
            Self {
                sender: Some(sender),
                wake: wake.clone(),
            },
            receiver,
            wake,
        ))
    }
}
#[cfg(not(windows))]
type PlatformSender = tokio::sync::mpsc::Sender<IconJob>;
#[cfg(windows)]
type PlatformSender = WindowsJobSender;
pub(crate) type JobReply = tokio::sync::oneshot::Sender<Result<IconImage, AlefError>>;
type JobAnswer = tokio::sync::oneshot::Receiver<Result<IconImage, AlefError>>;

pub(crate) struct IconService {
    sender: tokio::sync::OnceCell<PlatformSender>,
    starter: BlockingExecutor,
}
impl Default for IconService {
    fn default() -> Self {
        Self {
            sender: tokio::sync::OnceCell::new(),
            starter: BlockingExecutor::new(
                std::num::NonZeroUsize::new(1).expect("non-zero startup limit"),
            ),
        }
    }
}
impl IconService {
    async fn sender(&self) -> Result<PlatformSender, AlefError> {
        self.sender
            .get_or_try_init(|| async {
                #[cfg(windows)]
                let starter = self.starter.clone();
                #[cfg(windows)]
                let startup = starter.submit(move || {
                    let (tx, rx, wake) = WindowsJobSender::channel()?;
                    std::thread::Builder::new()
                        .name("shell-icons".into())
                        .spawn(move || crate::desktop::shell::icon_windows::worker_loop(rx, wake))
                        .map(|_| tx)
                        .map_err(|error| png_error(format!("worker spawn: {error}")))
                })?;
                #[cfg(not(windows))]
                let startup = {
                    let (tx, rx) = tokio::sync::mpsc::channel(32);
                    self.starter.submit(move || {
                        std::thread::Builder::new()
                            .name("shell-icons".into())
                            .spawn(move || worker_loop(rx))
                            .map(|_| tx)
                            .map_err(|error| png_error(format!("worker spawn: {error}")))
                    })?
                };
                startup.result().await?
            })
            .await
            .cloned()
    }
    async fn enqueue(&self, target: IconTarget, size: u32) -> Result<JobAnswer, AlefError> {
        let (reply, answer) = tokio::sync::oneshot::channel();
        self.sender()
            .await?
            .try_send(IconJob {
                target,
                size,
                reply,
            })
            .map_err(|error| match error {
                tokio::sync::mpsc::error::TrySendError::Full(_) => {
                    AlefError::new(ErrorCode::Busy, "icon worker queue is full")
                }
                tokio::sync::mpsc::error::TrySendError::Closed(_) => {
                    png_error("icon worker queue closed")
                }
            })?;
        Ok(answer)
    }
}

async fn await_icon(
    service: &IconService,
    target: IconTarget,
    size: u32,
) -> Result<IconImage, AlefError> {
    match tokio::time::timeout(Duration::from_secs(30), async {
        let answer = service.enqueue(target, size).await?;
        answer
            .await
            .map_err(|_| png_error("icon worker reply closed"))?
    })
    .await
    {
        Ok(result) => result,
        Err(_) => Err(png_error("icon worker did not answer in time")),
    }
}

pub(crate) trait IconBackend {
    fn render(&mut self, target: &IconTarget, size: u32) -> Result<IconImage, AlefError>;
}

pub(crate) fn run_worker_loop_with_receive<B: IconBackend>(
    mut receive: impl FnMut() -> Option<IconJob>,
    mut backend: B,
) {
    while let Some(job) = receive() {
        if job.reply.is_closed() {
            continue;
        }
        let result = backend.render(&job.target, job.size);
        let _ = job.reply.send(result);
    }
}
#[cfg(any(not(windows), test))]
pub(crate) fn run_worker_loop<B: IconBackend>(mut receiver: JobReceiver, backend: B) {
    run_worker_loop_with_receive(|| receiver.blocking_recv(), backend);
}
#[cfg(not(windows))]
pub(crate) fn worker_loop(receiver: JobReceiver) {
    platform::worker_loop(receiver)
}
mod platform {
    #[cfg(all(unix, not(target_os = "macos")))]
    pub(crate) use crate::desktop::shell::icon_linux::worker_loop;
    #[cfg(target_os = "macos")]
    pub(crate) use crate::desktop::shell::icon_macos::worker_loop;
    #[cfg(not(any(windows, target_os = "macos", all(unix, not(target_os = "macos")))))]
    pub(crate) fn worker_loop(mut receiver: super::JobReceiver) {
        super::run_worker_loop(receiver, FallbackBackend);
    }
    #[cfg(not(any(windows, target_os = "macos", all(unix, not(target_os = "macos")))))]
    struct FallbackBackend;
    #[cfg(not(any(windows, target_os = "macos", all(unix, not(target_os = "macos")))))]
    impl super::IconBackend for FallbackBackend {
        fn render(
            &mut self,
            _: &super::IconTarget,
            _: u32,
        ) -> Result<super::IconImage, alef_core::AlefError> {
            Ok(super::generic_fallback())
        }
    }
}

enum Prepared {
    Cached(Reply),
    Work(IconTarget, u32, Option<String>),
}
fn prepare(ctx: CallContext, args: FileIconArgs, cache: &IconCache) -> Result<Prepared, AlefError> {
    check_size(args.size)?;
    let target = match args.target {
        IconTarget::Path(PathTarget { path }) => {
            let (path, decision) =
                ctx.permissions
                    .authorize(Permission::FsRead, Some(&path), &ctx.grants())?;
            if decision == Decision::Substitute {
                return Ok(Prepared::Cached(icon_reply(generic_fallback())?));
            }
            super::existing(&path)?;
            IconTarget::Path(PathTarget {
                path: path.to_string_lossy().into_owned(),
            })
        }
        generic => validate_target(generic)?,
    };
    let key = generic_cache_key(&target, args.size);
    if let Some(image) = key.as_deref().and_then(|key| cache.get(key)) {
        return Ok(Prepared::Cached(icon_reply(image)?));
    }
    Ok(Prepared::Work(target, args.size, key))
}

fn icon_reply(image: IconImage) -> Result<Reply, AlefError> {
    use base64::{engine::general_purpose::STANDARD, Engine as _};
    Ok(Reply::Json(
        serde_json::json!({ "png": STANDARD.encode(&image.png), "width": image.width, "height": image.height }),
    ))
}

pub(crate) fn register(registry: &mut Registry) -> Result<(), AlefError> {
    let worker =
        BlockingExecutor::new(std::num::NonZeroUsize::new(32).expect("non-zero worker limit"));
    let service = Arc::new(IconService::default());
    let cache = Arc::new(IconCache::new(256));
    // Permission::None allows dispatch; FsRead for path targets is authorized inside prepare.
    registry
        .command::<FileIconArgs>("shell.fileIcon")?
        .waits()
        .substitutes()
        .handler(move |ctx, args| {
            let service = service.clone();
            let cache = cache.clone();
            let worker = worker.clone();
            async move {
                let (ctx, args, cache_for_prepare) = (ctx, args, cache.clone());
                let prepare_job =
                    worker.submit(move || prepare(ctx, args, cache_for_prepare.as_ref()))?;
                let prepared = prepare_job.result().await??;
                match prepared {
                    Prepared::Cached(reply) => Ok(reply),
                    Prepared::Work(target, size, key) => {
                        let image = await_icon(&service, target, size).await?;
                        if let Some(key) = key {
                            let cache_for_finish = cache.clone();
                            let image_for_cache = image.clone();
                            worker
                                .submit(move || cache_for_finish.put(key, image_for_cache))?
                                .result()
                                .await?;
                        }
                        worker.submit(move || icon_reply(image))?.result().await?
                    }
                }
            }
        })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    fn image() -> IconImage {
        generic_fallback()
    }

    #[test]
    fn an_unsupported_size_is_refused() {
        assert!(check_size(31).is_err());
        for &n in SIZES {
            assert!(check_size(n).is_ok());
        }
    }
    #[test]
    fn nearest_size_breaks_ties_downward() {
        assert_eq!(nearest_size(0), 16);
        assert_eq!(nearest_size(20), 16);
        assert_eq!(nearest_size(56), 48);
        assert_eq!(nearest_size(1000), 256);
    }
    #[test]
    fn rgba_png_round_trips_exact_pixels() {
        let pixels = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15, 16];
        let bytes = rgba_to_png(2, 2, &pixels).unwrap();
        assert_eq!(png_dimensions(&bytes), Some((2, 2)));
        let mut decoder = png::Decoder::new(Cursor::new(bytes));
        decoder.set_transformations(png::Transformations::IDENTITY);
        let mut reader = decoder.read_info().unwrap();
        let mut decoded = vec![
            0;
            reader
                .output_buffer_size()
                .expect("valid image buffer size")
        ];
        let info = reader.next_frame(&mut decoded).unwrap();
        assert_eq!(&decoded[..info.buffer_size()], &pixels);
    }
    #[test]
    fn rgba_buffer_length_overflow_and_mismatch_are_refused() {
        assert!(rgba_to_png(2, 2, &[0; 15]).is_err());
        assert!(rgba_to_png(u32::MAX, u32::MAX, &[]).is_err());
    }
    #[test]
    fn png_header_requires_signature_and_ihdr() {
        let b = rgba_to_png(3, 5, &[0; 60]).unwrap();
        assert_eq!(png_dimensions(&b), Some((3, 5)));
        assert_eq!(png_dimensions(b"garbage"), None);
        let mut bad = b;
        bad[12] = b'X';
        assert_eq!(png_dimensions(&bad), None);
    }
    #[test]
    fn premultiplied_bgra_becomes_rgba() {
        let mut p = [
            0x40, 0x80, 0xc0, 0xff, 0x80, 0x40, 0x00, 0x80, 0xff, 0xff, 0xff, 0, 0xff, 0x00, 0x80,
            0x40,
        ];
        unpremultiply_bgra_to_rgba(&mut p);
        assert_eq!(
            p,
            [0xc0, 0x80, 0x40, 0xff, 0, 127, 255, 0x80, 0, 0, 0, 0, 255, 0, 255, 0x40]
        );
    }
    #[test]
    fn mime_lookup_is_case_insensitive_and_strips_parameters() {
        assert_eq!(mime_to_extension("IMAGE/PNG"), Some("png"));
        assert_eq!(mime_to_extension("text/plain; charset=utf-8"), Some("txt"));
        assert_eq!(mime_to_extension("unknown/type"), None);
    }
    #[test]
    fn extension_key_strips_only_one_dot() {
        assert_eq!(extension_key(" PDF "), "pdf");
        assert_eq!(extension_key(".Txt"), "txt");
        assert_eq!(extension_key("..md"), ".md");
    }
    #[test]
    fn target_deserialization_rejects_ambiguous_or_empty_shapes() {
        assert!(serde_json::from_str::<FileIconArgs>(r#"{"target":{},"size":32}"#).is_err());
        assert!(
            serde_json::from_str::<FileIconArgs>(r#"{"target":{"path":"x","kind":"file"}}"#)
                .is_err()
        );
        assert!(serde_json::from_str::<FileIconArgs>(r#"{"target":{"extension":""}}"#).is_ok());
        assert!(validate_target(
            serde_json::from_str::<FileIconArgs>(r#"{"target":{"extension":""}}"#)
                .unwrap()
                .target
        )
        .is_err());
    }
    #[test]
    fn invalid_extensions_and_mimes_are_rejected() {
        assert!(validate_target(IconTarget::Extension(ExtensionTarget {
            extension: "../x".into()
        }))
        .is_err());
        assert!(validate_target(IconTarget::Mime(MimeTarget {
            mime: "text/../../x".into()
        }))
        .is_err());
        assert!(validate_target(IconTarget::Extension(ExtensionTarget {
            extension: " PDF ".into()
        }))
        .is_ok());
    }
    #[test]
    fn cache_keys_are_normalized_and_paths_are_not_cached() {
        assert_eq!(
            generic_cache_key(
                &IconTarget::Extension(ExtensionTarget {
                    extension: ".PDF".into()
                }),
                32
            ),
            Some("ext:pdf|32".into())
        );
        assert_eq!(
            generic_cache_key(&IconTarget::Path(PathTarget { path: "/x".into() }), 32),
            None
        );
        assert_eq!(
            generic_cache_key(
                &IconTarget::Kind(KindTarget {
                    kind: IconKind::Folder
                }),
                32
            ),
            Some("kind:folder|32".into())
        );
    }
    #[test]
    fn cache_miss_eviction_refresh_and_capacity_one_work() {
        let c = IconCache::new(2);
        assert!(c.get("miss").is_none());
        c.put("a".into(), image());
        c.put("b".into(), image());
        assert!(c.get("a").is_some());
        c.put("c".into(), image());
        assert!(c.get("a").is_some());
        assert!(c.get("b").is_none());
        let one = IconCache::new(1);
        one.put("a".into(), image());
        one.put("b".into(), image());
        assert!(one.get("a").is_none());
        assert!(one.get("b").is_some());
    }

    #[tokio::test]
    async fn bounded_queue_skips_cancelled_jobs_and_replies_to_live_ones() {
        use std::collections::VecDeque;
        use std::sync::{mpsc as std_mpsc, Arc};
        struct Fake {
            starts: VecDeque<tokio::sync::oneshot::Sender<()>>,
            releases: std_mpsc::Receiver<()>,
            calls: Arc<AtomicUsize>,
        }
        impl IconBackend for Fake {
            fn render(&mut self, _: &IconTarget, _: u32) -> Result<IconImage, AlefError> {
                self.calls.fetch_add(1, Ordering::SeqCst);
                if let Some(start) = self.starts.pop_front() {
                    let _ = start.send(());
                }
                self.releases
                    .recv_timeout(Duration::from_secs(2))
                    .expect("explicit release");
                Ok(generic_fallback())
            }
        }
        let (tx, rx) = tokio::sync::mpsc::channel(2);
        let (first_start_tx, first_start_rx) = tokio::sync::oneshot::channel();
        let (live_start_tx, live_start_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std_mpsc::sync_channel(2);
        let calls = Arc::new(AtomicUsize::new(0));
        let fake = Fake {
            starts: VecDeque::from([first_start_tx, live_start_tx]),
            releases: release_rx,
            calls: calls.clone(),
        };
        let worker = std::thread::spawn(move || run_worker_loop(rx, fake));
        let target = IconTarget::Kind(KindTarget {
            kind: IconKind::File,
        });
        let (first_reply, first_answer) = tokio::sync::oneshot::channel();
        tx.try_send(IconJob {
            target: target.clone(),
            size: 32,
            reply: first_reply,
        })
        .expect("first admitted");
        tokio::time::timeout(Duration::from_secs(2), first_start_rx)
            .await
            .expect("first starts")
            .expect("first signal");
        let (cancel_reply, cancel_answer) = tokio::sync::oneshot::channel();
        tx.try_send(IconJob {
            target: target.clone(),
            size: 32,
            reply: cancel_reply,
        })
        .expect("cancel candidate queued");
        drop(cancel_answer);
        let (live_reply, live_answer) = tokio::sync::oneshot::channel();
        tx.try_send(IconJob {
            target: target.clone(),
            size: 32,
            reply: live_reply,
        })
        .expect("live queued");
        let (extra_reply, _extra_answer) = tokio::sync::oneshot::channel();
        assert!(matches!(
            tx.try_send(IconJob {
                target,
                size: 32,
                reply: extra_reply
            }),
            Err(tokio::sync::mpsc::error::TrySendError::Full(_))
        ));
        release_tx.send(()).expect("release first render");
        tokio::time::timeout(Duration::from_secs(2), live_start_rx)
            .await
            .expect("live starts after canceled queue item")
            .expect("live signal");
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        release_tx.send(()).expect("release live render");
        assert!(tokio::time::timeout(Duration::from_secs(2), first_answer)
            .await
            .expect("first reply")
            .expect("first sender")
            .is_ok());
        assert!(tokio::time::timeout(Duration::from_secs(2), live_answer)
            .await
            .expect("live reply")
            .expect("live sender")
            .is_ok());
        drop(tx);
        tokio::time::timeout(
            Duration::from_secs(2),
            tokio::task::spawn_blocking(move || worker.join().expect("worker exits")),
        )
        .await
        .expect("worker joined")
        .expect("join task");
    }
}
