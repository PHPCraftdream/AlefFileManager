// SPDX-License-Identifier: MIT OR Apache-2.0
//! The camera, microphone and audio backends behind the `media` module, and the device streams
//! they hand out: a pacing thread produces frames or chunks into a bounded queue, the relay of the
//! module forwards them to the page.
use std::{
    fmt::Debug,
    panic::{catch_unwind, AssertUnwindSafe},
    sync::{mpsc::RecvTimeoutError, Arc, Mutex},
    time::{Duration, Instant},
};

use alef_core::{
    session::{BlockingExecutor, BlockingJob},
    AlefError, ErrorCode,
};

use super::queue::{CameraFrame, MediaQueue, PcmChunk, StreamItem};

/// The cameras of the machine. Calls may block briefly; the module runs them where it must.
pub trait CameraBackend: Send + Sync + Debug {
    /// The cameras the user has.
    fn list(&self) -> Result<Vec<CameraInfo>, AlefError>;
    /// Opens a camera; the returned stream runs until it is stopped.
    fn open(&self, id: &str, fps: f64) -> Result<Arc<DeviceStream<CameraFrame>>, AlefError>;
}

/// The microphones of the machine.
pub trait MicrophoneBackend: Send + Sync + Debug {
    /// The microphones the user has.
    fn list(&self) -> Result<Vec<MicrophoneInfo>, AlefError>;
    /// Opens a microphone; the returned stream runs until it is stopped, and the second value is
    /// the rate the chunks will actually carry — the device's own, which need not equal the
    /// requested one.
    fn open(
        &self,
        id: &str,
        frequency: f64,
        rate: u32,
    ) -> Result<(Arc<DeviceStream<PcmChunk>>, u32), AlefError>;
}

/// Where the page sends the audio it plays.
pub trait AudioBackend: Send + Sync + Debug {
    /// Opens a sink the page writes its samples to.
    fn open(&self) -> Result<AudioSink, AlefError>;
}

/// A camera the backend offered.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct CameraInfo {
    pub id: String,
    pub label: String,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
}

/// A microphone the backend offered.
#[derive(Debug, Clone, PartialEq, serde::Serialize)]
pub struct MicrophoneInfo {
    pub id: String,
    pub label: String,
    pub rate: u32,
    pub frequency: f64,
}

/// A live producer: the queue the relay reads, the stop signal and the blocking job that produces.
/// The job is admitted on a [`BlockingExecutor`] before it starts, so a full budget refuses the
/// producer with `Busy`.
pub struct DeviceStream<T: StreamItem> {
    queue: Arc<MediaQueue<T>>,
    job: Mutex<Option<BlockingJob<()>>>,
    stop: std::sync::mpsc::Sender<()>,
}

impl<T: StreamItem> std::fmt::Debug for DeviceStream<T> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DeviceStream")
            .finish_non_exhaustive()
    }
}

/// Runs producer cleanup once on normal exit, cancellation, or unwind.
struct ExitCleanup(Option<Box<dyn FnOnce() + Send>>);
impl Drop for ExitCleanup {
    fn drop(&mut self) {
        if let Some(cleanup) = self.0.take() {
            let _ = catch_unwind(AssertUnwindSafe(cleanup));
        }
    }
}

impl<T: StreamItem> Drop for DeviceStream<T> {
    fn drop(&mut self) {
        let _ = self.stop.send(());
        self.queue.close();
    }
}

impl<T: StreamItem> DeviceStream<T> {
    /// Starts a producer on the module executor; see [`DeviceStream::start_on`].
    pub fn start(
        capacity: usize,
        interval: Duration,
        on_exit: impl FnOnce() + Send + 'static,
        produce: impl FnMut(u64, &MediaQueue<T>) -> T + Send + 'static,
    ) -> Result<Arc<Self>, AlefError> {
        Self::start_on(
            super::super::executor(),
            capacity,
            interval,
            on_exit,
            produce,
        )
    }

    /// Starts a producer that calls `produce` once per `interval` (the first at once) and queues
    /// what it makes; `on_exit` runs when the job is over, however it ended. The producer sees the
    /// queue to stamp each item with the drops so far. Admission is decided before the producer
    /// starts: a full executor answers `Busy`.
    pub fn start_on(
        executor: &BlockingExecutor,
        capacity: usize,
        interval: Duration,
        on_exit: impl FnOnce() + Send + 'static,
        mut produce: impl FnMut(u64, &MediaQueue<T>) -> T + Send + 'static,
    ) -> Result<Arc<Self>, AlefError> {
        let queue = Arc::new(MediaQueue::new(capacity));
        let (stop, receiver) = std::sync::mpsc::channel();
        let queue_for_job = queue.clone();
        let cleanup = ExitCleanup(Some(Box::new(on_exit)));
        let job = executor.submit(move || {
            let _cleanup = cleanup;
            let mut seq = 0u64;
            let mut next = Instant::now() + interval;
            loop {
                if queue_for_job.is_closed() {
                    break;
                }
                seq += 1;
                let item = produce(seq, &queue_for_job);
                if queue_for_job.is_closed() {
                    break;
                }
                queue_for_job.push(item);
                let now = Instant::now();
                let wait = next.saturating_duration_since(now);
                next = next.max(now) + interval;
                match receiver.recv_timeout(wait) {
                    Ok(()) | Err(RecvTimeoutError::Disconnected) => break,
                    Err(RecvTimeoutError::Timeout) => {}
                }
            }
        })?;
        Ok(Arc::new(Self {
            queue,
            stop,
            job: Mutex::new(Some(job)),
        }))
    }

    /// The queue the relay reads.
    pub fn queue(&self) -> Arc<MediaQueue<T>> {
        self.queue.clone()
    }

    /// How many items were pushed and how many the queue dropped.
    pub fn stats(&self) -> (u64, u64) {
        (self.queue.pushed(), self.queue.drops())
    }

    /// Stops the producer; calling it again does nothing.
    pub fn stop(&self) {
        let _ = self.stop.send(());
        self.queue.close();
    }

    /// Takes the job of the producer, so a closer may wait for it.
    pub fn take_job(&self) -> Option<BlockingJob<()>> {
        self.job.lock().unwrap_or_else(|e| e.into_inner()).take()
    }
}

use super::state::{DeviceKind, Failure};

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// A test-only hook called at the start of every `write`, so a test can hold the fold while
/// chunks are already queued. Never set outside the unit tests.
#[cfg(test)]
type WriteGate = std::sync::Arc<dyn Fn() + Send + Sync>;

/// A count of the samples a page played and a checksum over their bytes; enough to compare what
/// two runs heard without keeping the audio.
#[derive(Clone)]
pub struct AudioSink {
    state: Arc<Mutex<SinkState>>,
    /// Test-only gate; never set outside the unit tests. The gate is shared by every clone of
    /// the sink, so gating any clone — including the fold's — blocks every write of the sink.
    #[cfg(test)]
    gate: std::sync::Arc<Mutex<Option<WriteGate>>>,
}

struct SinkState {
    samples: u64,
    checksum: u64,
    closed: bool,
    output: Mutex<Option<Arc<super::super::system::samples::PcmRing>>>,
    halt: Mutex<Option<Box<dyn FnOnce() + Send>>>,
    failure: Mutex<Option<(Arc<Failure>, DeviceKind)>>,
}

impl Default for SinkState {
    fn default() -> Self {
        Self {
            samples: 0,
            checksum: FNV_OFFSET,
            closed: false,
            output: Mutex::new(None),
            halt: Mutex::new(None),
            failure: Mutex::new(None),
        }
    }
}

impl Drop for SinkState {
    fn drop(&mut self) {
        if let Some(halt) = self
            .halt
            .get_mut()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            halt();
        }
    }
}

impl AudioSink {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(SinkState::default())),
            #[cfg(test)]
            gate: std::sync::Arc::new(Mutex::new(None)),
        }
    }

    /// Associates callback failure status with this sink.
    pub fn attach_failure(&self, failure: Arc<Failure>, kind: DeviceKind) {
        *self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .failure
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some((failure, kind));
    }

    /// Attaches an output ring; every written chunk is also pushed into it.
    pub fn attach_output(&self, ring: std::sync::Arc<super::super::system::samples::PcmRing>) {
        *self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .output
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(ring);
    }

    /// The attached output ring, if any; the system backend polls it instead of being driven.
    pub fn output(&self) -> Option<std::sync::Arc<super::super::system::samples::PcmRing>> {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .output
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Attaches a hook that `close` runs once, after it marked the sink closed; the system audio
    /// backend takes its operating system stream down through it.
    pub fn attach_halt(&self, halt: Box<dyn FnOnce() + Send>) {
        *self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .halt
            .lock()
            .unwrap_or_else(|e| e.into_inner()) = Some(halt);
    }

    /// Installs the test-only write gate; see [`WriteGate`].
    #[cfg(test)]
    pub fn set_write_gate(&self, gate: WriteGate) {
        *self.gate.lock().unwrap_or_else(|e| e.into_inner()) = Some(gate);
    }

    /// Accepts whole `f32` samples, folding them into the checksum; answers the cumulative count.
    pub fn write(&self, bytes: &[u8]) -> Result<u64, AlefError> {
        #[cfg(test)]
        {
            let gate = self.gate.lock().unwrap_or_else(|e| e.into_inner()).clone();
            if let Some(gate) = gate {
                gate();
            }
        }
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some((failure, kind)) = state
            .failure
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            if let Some(error) = failure.error(*kind) {
                return Err(error);
            }
        }
        if !bytes.len().is_multiple_of(4) {
            return Err(AlefError::new(
                ErrorCode::InvalidArgument,
                "the samples are not whole f32 values",
            ));
        }
        if state.closed {
            return Err(AlefError::new(
                ErrorCode::Closed,
                "the audio sink is closed already",
            ));
        }
        for byte in bytes {
            state.checksum ^= u64::from(*byte);
            state.checksum = state.checksum.wrapping_mul(FNV_PRIME);
        }
        state.samples += (bytes.len() / 4) as u64;
        if let Some(ring) = state
            .output
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
        {
            let samples: Vec<f32> = bytes
                .chunks_exact(4)
                .map(|chunk| f32::from_le_bytes(chunk.try_into().expect("whole f32")))
                .collect();
            ring.write(&samples);
        }
        Ok(state.samples)
    }

    /// Closes the sink and answers (samples, checksum). The attached halt hook, if any, runs once
    /// here, off the state lock.
    pub fn close(&self) -> (u64, u64) {
        self.try_close().unwrap_or_else(|_| self.stats())
    }

    /// Closes, runs the nonblocking halt hook, then returns a stored device failure if present.
    pub fn try_close(&self) -> Result<(u64, u64), AlefError> {
        let halt = {
            let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
            state.closed = true;
            let halt = state.halt.lock().unwrap_or_else(|e| e.into_inner()).take();
            (state.samples, state.checksum, halt)
        };
        if let Some(halt) = halt.2 {
            halt();
        }
        let result = (halt.0, halt.1);
        if let Some((failure, kind)) = self
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .failure
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
        {
            if let Some(error) = failure.error(*kind) {
                return Err(error);
            }
        }
        Ok(result)
    }

    /// The (samples, checksum) so far.
    pub fn stats(&self) -> (u64, u64) {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        (state.samples, state.checksum)
    }

    /// The checksum as hex, because a u64 does not survive a JSON number exactly.
    pub fn checksum_hex(checksum: u64) -> String {
        format!("{:016x}", checksum)
    }
}

impl Debug for AudioSink {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("AudioSink").finish_non_exhaustive()
    }
}

impl Default for AudioSink {
    fn default() -> Self {
        Self::new()
    }
}

/// The stand-in of a build without cameras: every call answers `NotAvailable`.
#[derive(Debug, Default)]
pub struct MissingCamera;

impl CameraBackend for MissingCamera {
    fn list(&self) -> Result<Vec<CameraInfo>, AlefError> {
        Err(AlefError::new(
            ErrorCode::NotAvailable,
            "no camera in this build",
        ))
    }

    fn open(&self, _id: &str, _fps: f64) -> Result<Arc<DeviceStream<CameraFrame>>, AlefError> {
        Err(AlefError::new(
            ErrorCode::NotAvailable,
            "no camera in this build",
        ))
    }
}

/// The stand-in of a build without microphones: every call answers `NotAvailable`.
#[derive(Debug, Default)]
pub struct MissingMicrophone;

impl MicrophoneBackend for MissingMicrophone {
    fn list(&self) -> Result<Vec<MicrophoneInfo>, AlefError> {
        Err(AlefError::new(
            ErrorCode::NotAvailable,
            "no microphone in this build",
        ))
    }

    fn open(
        &self,
        _id: &str,
        _frequency: f64,
        _rate: u32,
    ) -> Result<(Arc<DeviceStream<PcmChunk>>, u32), AlefError> {
        Err(AlefError::new(
            ErrorCode::NotAvailable,
            "no microphone in this build",
        ))
    }
}

/// The stand-in of a build without audio output: every call answers `NotAvailable`.
#[derive(Debug, Default)]
pub struct MissingAudio;

impl AudioBackend for MissingAudio {
    fn open(&self) -> Result<AudioSink, AlefError> {
        Err(AlefError::new(
            ErrorCode::NotAvailable,
            "no audio output in this build",
        ))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use bytes::Bytes;
    use std::num::NonZeroUsize;
    use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

    fn tiny_frame(seq: u64) -> CameraFrame {
        CameraFrame {
            width: 1,
            height: 1,
            seq,
            timestamp_ms: 0,
            dropped: 0,
            payload: Bytes::from_static(&[0, 0, 0, 255]),
        }
    }

    #[test]
    fn missing_backends_answer_not_available() {
        let camera_error = MissingCamera.list().expect_err("the camera must refuse");
        assert_eq!(camera_error.code, ErrorCode::NotAvailable);
        let camera_error = MissingCamera
            .open("x", 30.0)
            .expect_err("the camera must refuse");
        assert_eq!(camera_error.code, ErrorCode::NotAvailable);
        let microphone_error = MissingMicrophone
            .list()
            .expect_err("the microphone must refuse");
        assert_eq!(microphone_error.code, ErrorCode::NotAvailable);
        let microphone_error = MissingMicrophone
            .open("x", 440.0, 48000)
            .expect_err("the microphone must refuse");
        assert_eq!(microphone_error.code, ErrorCode::NotAvailable);
        let audio_error = MissingAudio.open().expect_err("the audio must refuse");
        assert_eq!(audio_error.code, ErrorCode::NotAvailable);
    }

    #[test]
    fn the_attached_halt_runs_once_on_close_and_never_on_write() {
        let runs = Arc::new(AtomicUsize::new(0));
        let counted = Arc::clone(&runs);
        let sink = AudioSink::new();
        sink.attach_halt(Box::new(move || {
            counted.fetch_add(1, Ordering::SeqCst);
        }));
        assert_eq!(sink.write(&[1, 2, 3, 4]).unwrap(), 1);
        assert_eq!(runs.load(Ordering::SeqCst), 0);
        sink.close();
        sink.close();
        assert_eq!(runs.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn sink_failure_is_sticky_for_write_and_close_and_halt_runs_once() {
        let sink = AudioSink::new();
        let failure = Arc::new(Failure::default());
        sink.attach_failure(failure.clone(), DeviceKind::Audio);
        let halts = Arc::new(AtomicUsize::new(0));
        let count = halts.clone();
        sink.attach_halt(Box::new(move || {
            count.fetch_add(1, Ordering::SeqCst);
        }));
        assert!(failure.set(super::super::state::DeviceFailure::Internal));
        for error in [
            sink.write(&[0; 4]).unwrap_err(),
            sink.try_close().unwrap_err(),
        ] {
            assert_eq!(error.code, ErrorCode::Internal);
            assert_eq!(error.message, "audio device failed");
        }
        assert_eq!(halts.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn sink_drop_halts_once_and_failure_precedes_closed() {
        let halts = Arc::new(AtomicUsize::new(0));
        {
            let sink = AudioSink::new();
            let failure = Arc::new(Failure::default());
            sink.attach_failure(failure.clone(), DeviceKind::Audio);
            let count = halts.clone();
            sink.attach_halt(Box::new(move || {
                count.fetch_add(1, Ordering::SeqCst);
            }));
            sink.close();
            failure.set(super::super::state::DeviceFailure::Internal);
            assert_eq!(sink.write(&[0; 4]).unwrap_err().code, ErrorCode::Internal);
        }
        assert_eq!(halts.load(Ordering::SeqCst), 1);
        let drops = Arc::new(AtomicUsize::new(0));
        let count = drops.clone();
        let sink = AudioSink::new();
        sink.attach_halt(Box::new(move || {
            count.fetch_add(1, Ordering::SeqCst);
        }));
        let clone = sink.clone();
        drop(sink);
        assert_eq!(drops.load(Ordering::SeqCst), 0);
        drop(clone);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn the_audio_sink_counts_samples_and_checksums_its_bytes() {
        let sink = AudioSink::new();
        assert_eq!(sink.stats(), (0, FNV_OFFSET));
        assert_eq!(sink.write(&[1, 2, 3, 4]).unwrap(), 1);
        assert_eq!(sink.write(&[5, 6, 7, 8]).unwrap(), 2);
        let mut checksum = FNV_OFFSET;
        for byte in [1u8, 2, 3, 4, 5, 6, 7, 8] {
            checksum ^= u64::from(byte);
            checksum = checksum.wrapping_mul(FNV_PRIME);
        }
        assert_eq!(sink.stats(), (2, checksum));
        let error = sink.write(&[1, 2, 3]).expect_err("3 bytes must be refused");
        assert_eq!(error.code, ErrorCode::InvalidArgument);
        assert_eq!(sink.close(), (2, checksum));
        let error = sink
            .write(&[1, 2, 3, 4])
            .expect_err("a closed sink must refuse");
        assert_eq!(error.code, ErrorCode::Closed);
        assert_eq!(sink.close(), (2, checksum));
    }

    #[test]
    fn the_sink_feeds_an_attached_ring_and_counts_the_overrun() {
        let ring = Arc::new(super::super::super::system::samples::PcmRing::new(2));
        let sink = AudioSink::new();
        sink.attach_output(ring.clone());
        let bytes: Vec<u8> = [1.0f32, 2.0, 3.0]
            .iter()
            .flat_map(|sample| sample.to_le_bytes())
            .collect();
        assert_eq!(sink.write(&bytes).unwrap(), 3);
        // Only the tail of the write fits: the last two samples survive, the first is the overrun.
        let mut heard = [0.0f32; 2];
        ring.read(&mut heard);
        assert_eq!(heard, [2.0, 3.0]);
        assert_eq!(ring.overruns(), 1);
        // The sink's own accounting is untouched by the ring: 3 samples, unchanged FNV fold.
        let mut checksum = FNV_OFFSET;
        for byte in bytes.iter() {
            checksum ^= u64::from(*byte);
            checksum = checksum.wrapping_mul(FNV_PRIME);
        }
        assert_eq!(sink.stats(), (3, checksum));
        // A sink without an attached ring is unaffected by writes.
        let bare = AudioSink::new();
        assert_eq!(bare.write(&bytes).unwrap(), 3);
        let mut checksum_bare = FNV_OFFSET;
        for byte in bytes.iter() {
            checksum_bare ^= u64::from(*byte);
            checksum_bare = checksum_bare.wrapping_mul(FNV_PRIME);
        }
        assert_eq!(bare.stats(), (3, checksum_bare));
    }

    #[tokio::test]
    async fn the_pacer_produces_the_first_frame_at_once_and_stops_promptly() {
        let exited = Arc::new(AtomicBool::new(false));
        let on_exit = {
            let exited = exited.clone();
            move || {
                exited.store(true, Ordering::SeqCst);
            }
        };
        let stream = DeviceStream::<CameraFrame>::start(
            2,
            Duration::from_millis(50),
            on_exit,
            |seq, _queue| tiny_frame(seq),
        )
        .expect("admission must succeed");
        let first =
            tokio::time::timeout(Duration::from_secs(2), async { stream.queue().pop().await })
                .await
                .expect("the first frame must arrive soon");
        // A loaded runner may evict the oldest frames before the first pop: only the newest of them
        // is left, so the seq is at least 1 (not exactly 1).
        assert!(first.expect("a frame").seq >= 1);
        stream.stop();
        let job = stream.take_job().expect("the job must still be there");
        tokio::time::timeout(Duration::from_secs(2), job.result())
            .await
            .expect("the job must stop soon")
            .expect("the job must not fail");
        assert!(exited.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn a_full_media_budget_refuses_a_new_producer_with_busy() {
        let executor = BlockingExecutor::new(NonZeroUsize::new(1).unwrap());
        let first = DeviceStream::start_on(
            &executor,
            2,
            Duration::from_millis(50),
            || {},
            |seq, _| tiny_frame(seq),
        )
        .expect("the first producer must be admitted");
        let second = DeviceStream::start_on(
            &executor,
            2,
            Duration::from_millis(50),
            || {},
            |seq, _| tiny_frame(seq),
        )
        .expect_err("a full budget must refuse the second producer");
        assert_eq!(second.code, ErrorCode::Busy);
        first.stop();
        tokio::time::timeout(
            Duration::from_secs(2),
            first.take_job().expect("the job must be there").result(),
        )
        .await
        .expect("the job must stop soon")
        .ok();
    }

    #[test]
    fn busy_producer_refusal_cleans_up_and_dropped_stream_stops_job() {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .unwrap();
        let _entered = runtime.enter();
        let executor = BlockingExecutor::new(NonZeroUsize::new(1).unwrap());
        let (started_tx, started_rx) = std::sync::mpsc::channel();
        let (exit_tx, exit_rx) = std::sync::mpsc::channel();
        let exit = Arc::new(std::sync::Mutex::new(Some(exit_tx)));
        let first = DeviceStream::start_on(
            &executor,
            1,
            Duration::from_secs(5),
            move || {
                if let Some(sender) = exit.lock().unwrap().take() {
                    let _ = sender.send(());
                }
            },
            move |seq, _| {
                let _ = started_tx.send(());
                tiny_frame(seq)
            },
        )
        .unwrap();
        started_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("producer starts");
        let refused_cleanup = Arc::new(AtomicUsize::new(0));
        let refused_cleanup_count = Arc::clone(&refused_cleanup);
        let err = DeviceStream::start_on(
            &executor,
            1,
            Duration::from_secs(1),
            move || {
                refused_cleanup_count.fetch_add(1, Ordering::SeqCst);
            },
            |seq, _| tiny_frame(seq),
        )
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::Busy);
        assert_eq!(refused_cleanup.load(Ordering::SeqCst), 1);
        drop(first);
        exit_rx
            .recv_timeout(Duration::from_secs(2))
            .expect("drop cleanup runs");
    }

    #[tokio::test]
    async fn stopping_a_producer_releases_its_admission_permit() {
        let executor = BlockingExecutor::new(NonZeroUsize::new(1).unwrap());
        let first = DeviceStream::start_on(
            &executor,
            2,
            Duration::from_millis(50),
            || {},
            |seq, _| tiny_frame(seq),
        )
        .expect("the first producer must be admitted");
        first.stop();
        let job = first.take_job().expect("the job must be there");
        tokio::time::timeout(Duration::from_secs(2), job.result())
            .await
            .expect("the job must stop soon")
            .expect("the job must not fail");
        let second = DeviceStream::start_on(
            &executor,
            2,
            Duration::from_millis(50),
            || {},
            |seq, _| tiny_frame(seq),
        )
        .expect("the released permit must admit a new producer");
        second.stop();
        tokio::time::timeout(
            Duration::from_secs(2),
            second.take_job().expect("the job must be there").result(),
        )
        .await
        .expect("the job must stop soon")
        .expect("the job must not fail");
    }
}
