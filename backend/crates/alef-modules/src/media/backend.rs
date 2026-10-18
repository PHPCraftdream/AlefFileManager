// SPDX-License-Identifier: MIT OR Apache-2.0
//! The camera, microphone and audio backends behind the `media` module, and the device streams
//! they hand out: a pacing thread produces frames or chunks into a bounded queue, the relay of the
//! module forwards them to the page.
use std::{
    fmt::Debug,
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
    /// Opens a microphone; the returned stream runs until it is stopped.
    fn open(
        &self,
        id: &str,
        frequency: f64,
        rate: u32,
    ) -> Result<Arc<DeviceStream<PcmChunk>>, AlefError>;
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
    stop: std::sync::mpsc::Sender<()>,
    job: Mutex<Option<BlockingJob<()>>>,
}

impl<T: StreamItem> std::fmt::Debug for DeviceStream<T> {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("DeviceStream")
            .finish_non_exhaustive()
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
        Self::start_on(super::executor(), capacity, interval, on_exit, produce)
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
        let job = executor.submit(move || {
            let mut seq = 0u64;
            let mut next = Instant::now() + interval;
            loop {
                seq += 1;
                let item = produce(seq, &queue_for_job);
                queue_for_job.push(item);
                let now = Instant::now();
                let wait = next.saturating_duration_since(now);
                next = next.max(now) + interval;
                match receiver.recv_timeout(wait) {
                    Ok(()) | Err(RecvTimeoutError::Disconnected) => break,
                    Err(RecvTimeoutError::Timeout) => {}
                }
            }
            on_exit();
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

const FNV_OFFSET: u64 = 0xcbf2_9ce4_8422_2325;
const FNV_PRIME: u64 = 0x0000_0100_0000_01b3;

/// A count of the samples a page played and a checksum over their bytes; enough to compare what
/// two runs heard without keeping the audio.
#[derive(Clone)]
pub struct AudioSink {
    state: Arc<Mutex<SinkState>>,
}

struct SinkState {
    samples: u64,
    checksum: u64,
    closed: bool,
}

impl Default for SinkState {
    fn default() -> Self {
        Self {
            samples: 0,
            checksum: FNV_OFFSET,
            closed: false,
        }
    }
}

impl AudioSink {
    pub fn new() -> Self {
        Self {
            state: Arc::new(Mutex::new(SinkState::default())),
        }
    }

    /// Accepts whole `f32` samples, folding them into the checksum; answers the cumulative count.
    pub fn write(&self, bytes: &[u8]) -> Result<u64, AlefError> {
        if !bytes.len().is_multiple_of(4) {
            return Err(AlefError::new(
                ErrorCode::InvalidArgument,
                "the samples are not whole f32 values",
            ));
        }
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
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
        Ok(state.samples)
    }

    /// Closes the sink and answers (samples, checksum).
    pub fn close(&self) -> (u64, u64) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.closed = true;
        (state.samples, state.checksum)
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
    ) -> Result<Arc<DeviceStream<PcmChunk>>, AlefError> {
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
    use std::sync::atomic::{AtomicBool, Ordering};

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
        assert_eq!(first.expect("a frame").seq, 1);
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
