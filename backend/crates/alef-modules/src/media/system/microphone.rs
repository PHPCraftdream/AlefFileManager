// SPDX-License-Identifier: MIT OR Apache-2.0
//! The system microphone: the real input devices of the machine through `cpal`. The first open
//! of a real device may trigger the operating system permission prompt — an independent second
//! gate after the manifest permission `media.microphone`.
//!
//! Every open owns one owner thread, following the camera pattern: the `cpal` `Stream` lives and
//! dies on that thread and is never moved out, so no `unsafe Send` is needed. The callback of the
//! operating system never blocks and never allocates: it down-mixes sample by sample into a fixed
//! stack buffer and pushes into the bounded ring, whose drop-oldest accounting absorbs a full
//! ring. A fatal backend error ends the stream instead of producing silence forever.
use std::{
    sync::{
        atomic::{AtomicBool, AtomicU64, Ordering},
        mpsc, Arc, Condvar, Mutex,
    },
    time::Duration,
};

use alef_core::{AlefError, ErrorCode};
use bytes::Bytes;
use cpal::{
    traits::{DeviceTrait, HostTrait, StreamTrait},
    SampleFormat, SizedSample, Stream, StreamConfig,
};

use crate::media::{
    device::backend::{DeviceStream, MicrophoneBackend, MicrophoneInfo},
    device::queue::{unix_ms, PcmChunk},
    device::state::{CpalFailureKind, DeviceFailure, DeviceKind, Failure, OwnerLease},
    invalid,
    system::samples::{chunk_samples, downmix_into, PcmRing},
};

/// Stable identifiers: `system-<index>`, the index of the device in the input enumeration of the
/// system. An `open` re-enumerates and takes the device at that index, so the id round-trips as
/// long as the set of devices does not change in between.
const ID_PREFIX: &str = "system-";
/// How long an open may take: a permission prompt fits in it.
const OPEN_TIMEOUT: Duration = Duration::from_secs(60);
/// The stack accumulator of the input callback: full frames of mono samples flushed into the
/// ring. Stack, not heap — the callback allocates nothing.
const SCRATCH: usize = 4096;

/// The microphone of a real device carries silence, not a sine: `frequency` applies only to the
/// virtual one. The `rate` argument is validated like the virtual microphone's but then ignored —
/// the configuration of the operating system decides, and the chunks carry the device's own rate.
#[derive(Debug, Default)]
pub struct SystemMicrophone;

impl MicrophoneBackend for SystemMicrophone {
    fn list(&self) -> Result<Vec<MicrophoneInfo>, AlefError> {
        // An empty list is a machine without microphones, not an error.
        let mut infos = Vec::new();
        for (index, device) in cpal::default_host()
            .input_devices()
            .map_err(|_| list_error())?
            .enumerate()
        {
            let rate = device
                .default_input_config()
                .ok()
                .map_or(48000, |config| config.sample_rate());
            infos.push(MicrophoneInfo {
                id: format!("{ID_PREFIX}{index}"),
                label: device.to_string(),
                rate,
                // The wire contract is shared with the virtual microphone, whose header carries a
                // sine frequency; a real device has none, so it stays at zero.
                frequency: 0.0,
            });
        }
        Ok(infos)
    }

    fn open(
        &self,
        id: &str,
        _frequency: f64,
        rate: u32,
    ) -> Result<(Arc<DeviceStream<PcmChunk>>, u32), AlefError> {
        let index = parse_id(id)?;
        if !(8000..=192000).contains(&rate) {
            return Err(invalid("rate must be between 8000 and 192000".to_owned()));
        }
        let worker = MicWorker::spawn(index, id.to_owned())?;
        // This runs on the module blocking executor, which is allowed to block; the bounded wait
        // covers a permission prompt.
        match worker.receipt.recv_timeout(OPEN_TIMEOUT) {
            Ok(Ok((actual_rate, ring))) => {
                let shared = Arc::clone(&worker.shared);
                let mut n_chunks = 0u64;
                let max_samples =
                    chunk_samples(0, actual_rate, crate::media::virtuals::CHUNK_MS) as usize + 1;
                let mut buf = vec![0.0f32; max_samples];
                let mut ended = false;
                DeviceStream::start(
                    crate::media::virtuals::CHUNK_QUEUE,
                    Duration::from_millis(crate::media::virtuals::CHUNK_MS),
                    move || worker.stop(),
                    move |seq, queue| {
                        if let Some(error) = shared.failure.error(DeviceKind::Microphone) {
                            queue.fail(error);
                            return silent_chunk(seq, queue, n_chunks, actual_rate);
                        }
                        if ended || shared.closed.load(Ordering::Acquire) {
                            ended = true;
                            queue.fail(DeviceFailure::NotAvailable.error(DeviceKind::Microphone));
                            return silent_chunk(seq, queue, n_chunks, actual_rate);
                        }
                        let count =
                            chunk_samples(n_chunks, actual_rate, crate::media::virtuals::CHUNK_MS)
                                as usize;
                        n_chunks += 1;
                        let samples = &mut buf[..count];
                        ring.try_read(samples);
                        let mut payload = Vec::with_capacity(samples.len() * 4);
                        for value in samples.iter() {
                            payload.extend_from_slice(&value.to_le_bytes());
                        }
                        PcmChunk {
                            seq,
                            timestamp_ms: unix_ms(),
                            dropped: queue.drops(),
                            rate: actual_rate,
                            samples: samples.len(),
                            payload: Bytes::from(payload),
                        }
                    },
                )
                .map(|stream| (stream, actual_rate))
            }
            Ok(Err(error)) => {
                worker.stop();
                Err(error)
            }
            Err(_) => {
                worker.stop();
                Err(AlefError::new(
                    ErrorCode::Internal,
                    "the microphone did not open in time",
                ))
            }
        }
    }
}

/// What the produce closure and the owner thread share: the end marker, the stop flag, the wake
/// signal of the parked owner and the count of chunks the full ring dropped. `Send + Sync` with
/// no device handle in it.
struct MicShared {
    closed: AtomicBool,
    stop: AtomicBool,
    available: Condvar,
    gate: Mutex<()>,
    dropped_chunks: AtomicU64,
    failure: Arc<Failure>,
}

impl MicShared {
    fn close(&self) {
        self.closed.store(true, Ordering::Release);
        self.available.notify_all();
    }
}

/// The handle on the owner thread: its join handle, the shared flags and the ring the callback
/// feeds. The join happens on a blocking-executor thread, which may block; it is bounded in
/// practice because the owner checks the stop flag on every wake.
struct MicWorker {
    shared: Arc<MicShared>,
    receipt: mpsc::Receiver<Result<(u32, Arc<PcmRing>), AlefError>>,
}

impl MicWorker {
    fn spawn(index: usize, id: String) -> Result<Self, AlefError> {
        let (sender, receipt) = mpsc::channel();
        // A provisional capacity; the owner thread replaces it once the device rate is known.
        let shared = Arc::new(MicShared {
            closed: AtomicBool::new(false),
            stop: AtomicBool::new(false),
            available: Condvar::new(),
            gate: Mutex::new(()),
            dropped_chunks: AtomicU64::new(0),
            failure: Arc::new(Failure::default()),
        });
        let thread_shared = Arc::clone(&shared);
        let lease = OwnerLease::acquire()?;
        std::thread::Builder::new()
            .name("alef-media-mic".to_owned())
            .spawn(move || {
                let _owner_lease = lease;
                run_owner(index, &id, thread_shared, sender);
            })
            .map_err(|_| {
                AlefError::new(
                    ErrorCode::Internal,
                    "the microphone owner thread could not start",
                )
            })?;
        Ok(Self { shared, receipt })
    }

    fn stop(self) {
        drop(self);
    }
}

impl Drop for MicWorker {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Release);
        self.shared.available.notify_all();
    }
}

/// The whole life of the microphone on its owner thread: enumerate, negotiate, build and play the
/// stream, then park until stopped or fatally wrong. The `Stream` never leaves this function; it
/// drops here when the park ends, which stops the callback of the operating system.
fn run_owner(
    index: usize,
    id: &str,
    shared: Arc<MicShared>,
    sender: mpsc::Sender<Result<(u32, Arc<PcmRing>), AlefError>>,
) {
    let stream = match open_on_owner(index, id, &shared, &sender) {
        Ok(stream) => stream,
        // Nothing to keep alive: mark the end so a late consumer never waits on silence.
        Err(error) => {
            shared.close();
            let _ = sender.send(Err(error));
            return;
        }
    };
    // The owner owns the stream from here on; the park below keeps it alive until stop.
    let mut stream = Some(stream);
    let mut guard = shared.gate.lock().unwrap_or_else(|e| e.into_inner());
    while !shared.stop.load(Ordering::Relaxed) && !shared.closed.load(Ordering::Relaxed) {
        let (next, _) = shared
            .available
            .wait_timeout(guard, Duration::from_millis(500))
            .unwrap_or_else(|e| e.into_inner());
        guard = next;
    }
    // Drops ON the owner thread; the WASAPI drop joins its own worker internally, which is fine
    // on this dedicated thread. #5: nothing here ever drops the stream inside a callback.
    drop(stream.take());
}

/// Enumerates, negotiates, builds and plays the input stream on the owner thread. The ring is
/// created here at the device rate and published on the worker before the receipt is sent.
fn open_on_owner(
    index: usize,
    _id: &str,
    shared: &Arc<MicShared>,
    sender: &mpsc::Sender<Result<(u32, Arc<PcmRing>), AlefError>>,
) -> Result<Stream, AlefError> {
    let device = cpal::default_host()
        .input_devices()
        .map_err(|error| map_error_kind(error.kind()))?
        .nth(index)
        // The parse and the enumeration race: the device may vanish in between.
        .ok_or_else(|| AlefError::new(ErrorCode::NotFound, "the microphone is not available"))?;
    let config = device
        .default_input_config()
        .map_err(|error| map_error_kind(error.kind()))?;
    let format = config.sample_format();
    if !matches!(
        format,
        SampleFormat::F32 | SampleFormat::I16 | SampleFormat::U16 | SampleFormat::I32
    ) {
        return Err(AlefError::new(
            ErrorCode::NotAvailable,
            "the microphone sample format is not supported",
        ));
    }
    let stream_config = config.config();
    let channels = stream_config.channels;
    if channels == 0 {
        return Err(AlefError::new(ErrorCode::NotAvailable, MICROPHONE_TEXT));
    }
    let actual_rate = config.sample_rate();
    let per_chunk =
        (chunk_samples(0, actual_rate, crate::media::virtuals::CHUNK_MS) as usize).max(1);
    let ring = Arc::new(PcmRing::new(per_chunk * 4));
    let stream = match format {
        SampleFormat::F32 => {
            build_input::<f32>(&device, &stream_config, &ring, shared, f32_identity)
        }
        SampleFormat::I16 => build_input::<i16>(
            &device,
            &stream_config,
            &ring,
            shared,
            super::samples::f32_from_i16_one,
        ),
        SampleFormat::U16 => build_input::<u16>(
            &device,
            &stream_config,
            &ring,
            shared,
            super::samples::f32_from_u16_one,
        ),
        SampleFormat::I32 => build_input::<i32>(
            &device,
            &stream_config,
            &ring,
            shared,
            super::samples::f32_from_i32_one,
        ),
        _ => unreachable!("the format was checked above"),
    }?;
    stream
        .play()
        .map_err(|error| map_error_kind(error.kind()))?;
    sender
        .send(Ok((actual_rate, ring)))
        .map_err(|_| AlefError::new(ErrorCode::Internal, "the microphone did not open in time"))?;
    Ok(stream)
}

/// Builds the input stream in its native sample format `T`. The callback converts sample by
/// sample, averages each frame down to mono, accumulates into a fixed stack buffer and pushes
/// into the ring: no lock held across a wait, no heap allocation, no blocking.
fn build_input<T: SizedSample + 'static>(
    device: &cpal::Device,
    config: &StreamConfig,
    ring: &Arc<PcmRing>,
    shared: &Arc<MicShared>,
    convert: fn(T) -> f32,
) -> Result<Stream, AlefError> {
    let ring = Arc::clone(ring);
    let error_shared = Arc::clone(shared);
    let callback_shared = Arc::clone(shared);
    let channels = usize::from(config.channels.max(1));
    let config = *config;
    device
        .build_input_stream::<T, _, _>(
            config,
            move |data: &[T], _| {
                let mut mono = [0.0f32; SCRATCH];
                for block in data.chunks(SCRATCH * channels) {
                    let frames = downmix_into(block, channels, &mut mono, convert);
                    if frames > 0 && !ring.try_write(&mono[..frames]) {
                        callback_shared
                            .dropped_chunks
                            .fetch_add(1, Ordering::Relaxed);
                    }
                }
            },
            move |error| {
                error_shared
                    .failure
                    .set(DeviceFailure::from_cpal(match error.kind() {
                        cpal::ErrorKind::PermissionDenied => CpalFailureKind::PermissionDenied,
                        cpal::ErrorKind::DeviceNotAvailable => CpalFailureKind::DeviceNotAvailable,
                        _ => CpalFailureKind::Other,
                    }));
                error_shared.close();
            },
            None,
        )
        .map_err(|error| map_error_kind(error.kind()))
}

fn silent_chunk(
    seq: u64,
    queue: &crate::media::device::queue::MediaQueue<PcmChunk>,
    n_chunks: u64,
    rate: u32,
) -> PcmChunk {
    let count = chunk_samples(n_chunks, rate, crate::media::virtuals::CHUNK_MS) as usize;
    PcmChunk {
        seq,
        timestamp_ms: unix_ms(),
        dropped: queue.drops(),
        rate,
        samples: count,
        payload: Bytes::from(vec![0u8; count * 4]),
    }
}

fn f32_identity(value: f32) -> f32 {
    value
}

fn parse_id(id: &str) -> Result<usize, AlefError> {
    id.strip_prefix(ID_PREFIX)
        .and_then(|index| index.parse::<usize>().ok())
        .ok_or_else(|| AlefError::new(ErrorCode::NotFound, format!("unknown microphone {id}")))
}

/// Maps an open failure from the error kind alone. macOS asks the operating system for
/// microphone access and answers a denial with `PermissionDenied`; every other kind stays
/// `NotAvailable` with the same text, because cpal gives no better distinction for a real
/// device that will not open. Pure, so it is testable on every platform without a device.
fn map_error_kind(kind: cpal::ErrorKind) -> AlefError {
    let category = match kind {
        cpal::ErrorKind::PermissionDenied => CpalFailureKind::PermissionDenied,
        cpal::ErrorKind::DeviceNotAvailable => CpalFailureKind::DeviceNotAvailable,
        _ => CpalFailureKind::Other,
    };
    match DeviceFailure::from_cpal(category) {
        DeviceFailure::PermissionDenied => AlefError::new(
            ErrorCode::PermissionDenied,
            "the operating system denied the microphone",
        ),
        DeviceFailure::NotAvailable | DeviceFailure::Internal => {
            AlefError::new(ErrorCode::NotAvailable, MICROPHONE_TEXT)
        }
    }
}

const MICROPHONE_TEXT: &str = "the microphone is not available";

/// The system enumeration itself failed.
fn list_error() -> AlefError {
    AlefError::new(ErrorCode::Internal, "the microphones could not be listed")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_permission_denial_maps_to_permission_denied() {
        let error = map_error_kind(cpal::ErrorKind::PermissionDenied);
        assert_eq!(error.code, ErrorCode::PermissionDenied);
        assert_eq!(error.message, "the operating system denied the microphone");
    }

    #[test]
    fn a_missing_device_maps_to_not_available() {
        let error = map_error_kind(cpal::ErrorKind::DeviceNotAvailable);
        assert_eq!(error.code, ErrorCode::NotAvailable);
        assert_eq!(error.message, "the microphone is not available");
    }

    #[test]
    fn every_other_kind_stays_not_available_with_the_same_text() {
        // cpal gives no better distinction than the kind; the text must not leak the kind.
        for kind in [
            cpal::ErrorKind::BackendError,
            cpal::ErrorKind::UnsupportedConfig,
            cpal::ErrorKind::Other,
            cpal::ErrorKind::HostUnavailable,
        ] {
            let error = map_error_kind(kind);
            assert_eq!(error.code, ErrorCode::NotAvailable);
            assert_eq!(error.message, "the microphone is not available");
        }
    }

    #[test]
    fn an_unparsable_id_is_not_found_with_the_device_in_the_text() {
        // Kills a mutation of the fixed NotFound text or the prefix strip.
        let error = parse_id("virtual-3").expect_err("the foreign id must be refused");
        assert_eq!(error.code, ErrorCode::NotFound);
        assert_eq!(error.message, "unknown microphone virtual-3");
        let error = parse_id("system-x").expect_err("the bad index must be refused");
        assert_eq!(error.code, ErrorCode::NotFound);
        assert_eq!(error.message, "unknown microphone system-x");
        assert_eq!(parse_id("system-7").unwrap(), 7);
    }
}
