// SPDX-License-Identifier: MIT OR Apache-2.0
//! The system audio output: the default playback device through `cpal`. The sink counts what the
//! page writes and feeds the same samples into a bounded ring that the callback of the operating
//! system drains into the device; underruns answer silence. Every open owns one owner thread,
//! following the camera and microphone pattern: the `cpal` `Stream` lives and dies on that
//! thread and is never moved out, and closing the sink only sends a non-blocking stop signal, so
//! the stream drops ON the owner thread — never inside a callback or a halt hook.
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        mpsc, Arc,
    },
    time::Duration,
};

use alef_core::{AlefError, ErrorCode};
use cpal::{
    traits::{DeviceTrait, HostTrait, StreamTrait},
    SampleFormat, SizedSample, Stream, StreamConfig,
};

use crate::media::{
    device::{
        backend::{AudioBackend, AudioSink},
        state::{CpalFailureKind, DeviceFailure, DeviceKind, Failure, OwnerLease},
    },
    system::samples::{upmix_into, PcmRing},
};

/// How long an open may take.
const OPEN_TIMEOUT: Duration = Duration::from_secs(60);
/// The stack accumulator of the output callback: frames of mono samples pulled per pass. Stack,
/// not heap — the callback allocates nothing.
const SCRATCH: usize = 4096;

/// What the owner thread reports over the receipt: the device rate and channel count and the
/// ring its callback feeds.
type OpenedOutput = Result<(u32, u16, Arc<PcmRing>), AlefError>;

/// The default output device of the machine. The returned sink counts and checksums what the page
/// writes and feeds the same samples into the output ring that the callback of the operating
/// system drains into the device; underruns answer silence, the ring drops the oldest samples
/// when the device falls behind and counts them. Closing the sink sends a non-blocking stop to
/// the owner thread, which drops the `cpal` stream on itself and stops the callback.
#[derive(Debug, Default)]
pub struct SystemAudio;

impl AudioBackend for SystemAudio {
    fn open(&self) -> Result<AudioSink, AlefError> {
        let (notify, receipt) = mpsc::channel::<Result<(u32, u16, Arc<PcmRing>), AlefError>>();
        let (halt, stop) = mpsc::channel::<()>();
        let owner = AudioOwner::spawn(notify, stop)?;
        // This runs on the module blocking executor, which is allowed to block; the bounded wait
        // keeps a wedged device from hanging the open forever.
        let (_rate, _channels, ring) = match receipt.recv_timeout(OPEN_TIMEOUT) {
            Ok(Ok(opened)) => opened,
            Ok(Err(error)) => {
                owner.stop();
                return Err(error);
            }
            Err(_) => {
                owner.stop();
                return Err(AlefError::new(
                    ErrorCode::Internal,
                    "the audio output did not open in time",
                ));
            }
        };
        let sink = AudioSink::new();
        sink.attach_output(ring);
        sink.attach_failure(Arc::clone(&owner.shared.failure), DeviceKind::Audio);
        let shared = Arc::clone(&owner.shared);
        sink.attach_halt(Box::new(move || {
            shared.stop();
            let _ = halt.send(());
            drop(owner);
        }));
        // The negotiated geometry stays with the owner thread; the ring is what matters here.
        Ok(sink)
    }
}

/// What the owner thread shares with the callbacks: the stop flag, set by the consumer or by the
/// fatal-error path; the owner then drops the stream and exits.
struct AudioShared {
    stop: AtomicBool,
    failure: Arc<Failure>,
}

impl AudioShared {
    fn stop(&self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

/// A handle on the owner thread. Dropping it signals the owner without joining it.
struct AudioOwner {
    shared: Arc<AudioShared>,
}

impl AudioOwner {
    fn spawn(
        notify: mpsc::Sender<OpenedOutput>,
        stop: mpsc::Receiver<()>,
    ) -> Result<Self, AlefError> {
        let shared = Arc::new(AudioShared {
            stop: AtomicBool::new(false),
            failure: Arc::new(Failure::default()),
        });
        let thread_shared = Arc::clone(&shared);
        let lease = OwnerLease::acquire()?;
        std::thread::Builder::new()
            .name("alef-media-audio".to_owned())
            .spawn(move || {
                let _owner_lease = lease;
                run_owner(thread_shared, notify, stop);
            })
            .map_err(|_| {
                AlefError::new(
                    ErrorCode::Internal,
                    "the audio owner thread could not start",
                )
            })?;
        Ok(Self { shared })
    }

    fn stop(self) {
        drop(self);
    }
}

impl Drop for AudioOwner {
    fn drop(&mut self) {
        self.shared.stop();
    }
}

/// The whole life of the output on its owner thread: open, negotiate, build and play the stream,
/// then park until stopped or fatally wrong. The `Stream` never leaves this function; it drops
/// here when the park ends, which stops the callback of the operating system.
fn run_owner(
    shared: Arc<AudioShared>,
    notify: mpsc::Sender<OpenedOutput>,
    stop: mpsc::Receiver<()>,
) {
    let stream = match open_on_owner(&shared, &notify) {
        Ok(stream) => stream,
        Err(error) => {
            shared.stop();
            let _ = notify.send(Err(error));
            return;
        }
    };
    // The owner owns the stream from here on; the park below keeps it alive until stop.
    let mut stream = Some(stream);
    loop {
        // A stop from the halt hook, a fatal backend error, or a vanished receiver ends the
        // park; the timeout keeps the fatal flag observed even if no stop was ever sent.
        match stop.recv_timeout(Duration::from_millis(200)) {
            Ok(()) | Err(mpsc::RecvTimeoutError::Disconnected) => break,
            Err(mpsc::RecvTimeoutError::Timeout) => {}
        }
        if shared.stop.load(Ordering::Relaxed) {
            break;
        }
    }
    // Drops ON the owner thread; native backends may perform internal teardown here, never in a
    // callback or on the sink's close/drop caller.
    drop(stream.take());
}

/// Enumerates, negotiates, builds and plays the output stream on the owner thread. The ring is
/// created here at the device rate and published with the receipt.
fn open_on_owner(
    shared: &Arc<AudioShared>,
    notify: &mpsc::Sender<OpenedOutput>,
) -> Result<Stream, AlefError> {
    let device = cpal::default_host()
        .default_output_device()
        .ok_or_else(|| AlefError::new(ErrorCode::NotAvailable, NO_AUDIO_TEXT))?;
    let config = device
        .default_output_config()
        .map_err(|error| map_error_kind(error.kind()))?;
    let format = config.sample_format();
    if !matches!(
        format,
        SampleFormat::F32 | SampleFormat::I16 | SampleFormat::U16 | SampleFormat::I32
    ) {
        return Err(AlefError::new(
            ErrorCode::NotAvailable,
            "the audio output sample format is not supported",
        ));
    }
    let stream_config = config.config();
    let channels = stream_config.channels;
    if channels == 0 {
        return Err(AlefError::new(ErrorCode::NotAvailable, NO_AUDIO_TEXT));
    }
    let rate = config.sample_rate();
    let ring = Arc::new(PcmRing::new((rate as usize / 2).max(1)));
    let stream = match format {
        SampleFormat::F32 => {
            build_output::<f32>(&device, &stream_config, &ring, shared, |value| value)
        }
        SampleFormat::I16 => build_output::<i16>(
            &device,
            &stream_config,
            &ring,
            shared,
            crate::media::system::samples::i16_from_f32_one,
        ),
        SampleFormat::U16 => build_output::<u16>(
            &device,
            &stream_config,
            &ring,
            shared,
            crate::media::system::samples::u16_from_f32_one,
        ),
        SampleFormat::I32 => build_output::<i32>(
            &device,
            &stream_config,
            &ring,
            shared,
            crate::media::system::samples::i32_from_f32_one,
        ),
        _ => unreachable!("the format was checked above"),
    }?;
    stream
        .play()
        .map_err(|error| map_error_kind(error.kind()))?;
    notify.send(Ok((rate, channels, ring))).map_err(|_| {
        AlefError::new(ErrorCode::Internal, "the audio output did not open in time")
    })?;
    Ok(stream)
}

/// Builds the output stream in its native sample format `T`. The callback pulls mono frames from
/// the ring, up-mixes by writing the same sample to every channel slot and encodes per sample:
/// no lock held across a wait, no heap allocation, no blocking. A contended or short read
/// answers silence for that pass and counts one underrun inside the ring.
fn build_output<T: SizedSample + 'static>(
    device: &cpal::Device,
    config: &StreamConfig,
    ring: &Arc<PcmRing>,
    shared: &Arc<AudioShared>,
    encode: fn(f32) -> T,
) -> Result<Stream, AlefError> {
    let ring = Arc::clone(ring);
    let callback_shared = Arc::clone(shared);
    let callback_failure = Arc::clone(&shared.failure);
    let channels = usize::from(config.channels.max(1));
    let config = *config;
    device
        .build_output_stream::<T, _, _>(
            config,
            move |data: &mut [T], _| {
                drain_into(data, &ring, channels, encode);
            },
            move |error| {
                callback_failure.set(DeviceFailure::from_cpal(match error.kind() {
                    cpal::ErrorKind::PermissionDenied => CpalFailureKind::PermissionDenied,
                    cpal::ErrorKind::DeviceNotAvailable => CpalFailureKind::DeviceNotAvailable,
                    _ => CpalFailureKind::Other,
                }));
                callback_shared.stop();
            },
            None,
        )
        .map_err(|error| map_error_kind(error.kind()))
}

/// The body of the output callback, factored out so the tests can drive it device-free. Pulls
/// mono frames from the ring, up-mixes by writing the same sample to every channel slot and
/// encodes per sample: no lock held across a wait, no heap allocation, no blocking. A contended
/// or short read answers silence for that pass and counts one underrun inside the ring.
fn drain_into<T: SizedSample>(
    data: &mut [T],
    ring: &PcmRing,
    channels: usize,
    encode: fn(f32) -> T,
) {
    data.fill(encode(0.0));
    let channels = channels.max(1);
    let mut mono = [0.0f32; SCRATCH];
    for block in data.chunks_mut(SCRATCH * channels) {
        let frames = block.len() / channels;
        if frames == 0 {
            continue;
        }
        if ring.try_read(&mut mono[..frames]) {
            upmix_into(&mono[..frames], channels, block, encode);
        }
    }
}

/// Maps an open failure from the error kind alone. A denial answers `PermissionDenied`; every
/// other kind stays `NotAvailable` with the same text, because cpal gives no better distinction
/// for an output that will not open. Pure, so it is testable on every platform.
fn map_error_kind(kind: cpal::ErrorKind) -> AlefError {
    let category = match kind {
        cpal::ErrorKind::PermissionDenied => CpalFailureKind::PermissionDenied,
        cpal::ErrorKind::DeviceNotAvailable => CpalFailureKind::DeviceNotAvailable,
        _ => CpalFailureKind::Other,
    };
    match DeviceFailure::from_cpal(category) {
        DeviceFailure::PermissionDenied => AlefError::new(
            ErrorCode::PermissionDenied,
            "the operating system denied the audio output",
        ),
        DeviceFailure::NotAvailable => AlefError::new(ErrorCode::NotAvailable, NO_AUDIO_TEXT),
        DeviceFailure::Internal => AlefError::new(ErrorCode::NotAvailable, NO_AUDIO_TEXT),
    }
}

const NO_AUDIO_TEXT: &str = "no audio output is available";

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_permission_denial_maps_to_permission_denied() {
        let error = map_error_kind(cpal::ErrorKind::PermissionDenied);
        assert_eq!(error.code, ErrorCode::PermissionDenied);
        assert_eq!(
            error.message,
            "the operating system denied the audio output"
        );
    }

    #[test]
    fn a_missing_device_maps_to_not_available() {
        let error = map_error_kind(cpal::ErrorKind::DeviceNotAvailable);
        assert_eq!(error.code, ErrorCode::NotAvailable);
        assert_eq!(error.message, "no audio output is available");
    }

    #[test]
    fn every_other_kind_stays_not_available_with_the_same_text() {
        for kind in [
            cpal::ErrorKind::BackendError,
            cpal::ErrorKind::UnsupportedConfig,
            cpal::ErrorKind::Other,
            cpal::ErrorKind::HostUnavailable,
        ] {
            let error = map_error_kind(kind);
            assert_eq!(error.code, ErrorCode::NotAvailable);
            assert_eq!(error.message, "no audio output is available");
        }
    }

    #[test]
    fn the_output_callback_up_mixes_from_the_ring_and_silences_on_contention() {
        // Drives the exact production callback body (`drain_into`) through a stand-in ring:
        // frames are pulled as mono and written to every channel slot; a held ring mutex
        // (contention) must answer silence for the pass instead of blocking.
        let ring = std::sync::Arc::new(PcmRing::new(8));
        ring.write(&[0.5, -0.5]);
        let mut data = [0i16; 4];
        drain_into(
            &mut data,
            &ring,
            2,
            crate::media::system::samples::i16_from_f32_one,
        );
        assert_eq!(
            data,
            [i16_from(0.5), i16_from(0.5), i16_from(-0.5), i16_from(-0.5)]
        );
        // The contended pass runs on its own thread with a bounded wait: a callback that blocked
        // on the held mutex would fail the test instead of hanging it.
        let guard = ring.lock_for_test();
        let (sender, receiver) = std::sync::mpsc::channel();
        let worker = {
            let ring = ring.clone();
            std::thread::spawn(move || {
                let mut data = [7i16; 4];
                drain_into(
                    &mut data,
                    &ring,
                    2,
                    crate::media::system::samples::i16_from_f32_one,
                );
                let _ = sender.send(data);
            })
        };
        let data = receiver
            .recv_timeout(std::time::Duration::from_secs(2))
            .expect("the output callback blocked on a held ring");
        drop(guard);
        worker.join().expect("the callback thread must not panic");
        assert_eq!(data, [0, 0, 0, 0]);
    }

    fn i16_from(value: f32) -> i16 {
        crate::media::system::samples::i16_from_f32_one(value)
    }
}
