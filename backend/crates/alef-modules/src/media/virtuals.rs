// SPDX-License-Identifier: MIT OR Apache-2.0
//! Deterministic virtual devices: a camera that paints a moving test pattern, a microphone that
//! sings a sine and an audio sink that counts samples. They serve the end-to-end runs and the
//! tests; nothing here touches real hardware.
use std::{
    f64::consts::TAU,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc,
    },
    time::Duration,
};

use alef_core::{AlefError, ErrorCode};
use bytes::Bytes;

use super::{
    device::backend::{
        AudioBackend, AudioSink, CameraBackend, CameraInfo, DeviceStream, MicrophoneBackend,
    },
    device::queue::{CameraFrame, PcmChunk},
};

pub const VIRTUAL_CAMERA: &str = "virtual-0";
pub const VIRTUAL_MICROPHONE: &str = "virtual-0";
pub const VIRTUAL_WIDTH: u32 = 1280;
pub const VIRTUAL_HEIGHT: u32 = 720;
pub const VIRTUAL_RATE: u32 = 48000;
/// The length of one microphone chunk.
pub const CHUNK_MS: u64 = 20;
pub const FRAME_QUEUE: usize = 3;
pub const CHUNK_QUEUE: usize = 8;

/// A camera that paints full HD frames of a moving pattern at the asked rate.
#[derive(Debug, Default)]
pub struct VirtualCamera {
    streams: Arc<AtomicU64>,
}

impl VirtualCamera {
    /// How many streams are open right now.
    pub fn active_streams(&self) -> u64 {
        self.streams.load(Ordering::SeqCst)
    }
}

impl CameraBackend for VirtualCamera {
    fn list(&self) -> Result<Vec<CameraInfo>, AlefError> {
        Ok(vec![CameraInfo {
            id: VIRTUAL_CAMERA.to_owned(),
            label: "Alef virtual camera".to_owned(),
            width: VIRTUAL_WIDTH,
            height: VIRTUAL_HEIGHT,
            fps: 30.0,
        }])
    }

    fn open(&self, id: &str, fps: f64) -> Result<Arc<DeviceStream<CameraFrame>>, AlefError> {
        if id != VIRTUAL_CAMERA {
            return Err(AlefError::new(
                ErrorCode::NotFound,
                format!("unknown camera {id}"),
            ));
        }
        if !fps.is_finite() || !(1.0..=120.0).contains(&fps) {
            return Err(substituted_invalid(
                "fps must be a number between 1 and 120",
            ));
        }
        let streams = self.streams.clone();
        streams.fetch_add(1, Ordering::SeqCst);
        // The exit callback runs on every path, a refused admission included (the dropped
        // closure owns it): no rollback here, or the count would be taken back twice.
        DeviceStream::start(
            FRAME_QUEUE,
            Duration::from_secs_f64(1.0 / fps),
            move || {
                streams.fetch_sub(1, Ordering::SeqCst);
            },
            move |seq, queue| CameraFrame {
                width: VIRTUAL_WIDTH,
                height: VIRTUAL_HEIGHT,
                seq,
                timestamp_ms: super::device::queue::unix_ms(),
                dropped: queue.drops(),
                payload: paint(VIRTUAL_WIDTH, VIRTUAL_HEIGHT, seq),
            },
        )
    }
}

fn substituted_invalid(message: &str) -> AlefError {
    AlefError::new(ErrorCode::InvalidArgument, message.to_owned())
}

/// A frame of the moving pattern: each row is one 256-pixel block, already shaded for that row,
/// copied across the width — cheap even in debug builds.
fn paint(width: u32, height: u32, seq: u64) -> Bytes {
    debug_assert!((width as usize).is_multiple_of(256));
    let row_bytes = width as usize * 4;
    let mut frame = vec![0u8; row_bytes * height as usize];
    for (y, row) in frame.chunks_exact_mut(row_bytes).enumerate() {
        let green = (y as u8).wrapping_add((seq.wrapping_mul(5)) as u8);
        let mut block = [0u8; 256 * 4];
        for (x, pixel) in block.chunks_exact_mut(4).enumerate() {
            pixel[0] = (x as u8).wrapping_add((seq.wrapping_mul(3)) as u8);
            pixel[1] = green;
            pixel[2] = (seq.wrapping_mul(11)) as u8;
            pixel[3] = 255;
        }
        for chunk in row.chunks_exact_mut(block.len()) {
            chunk.copy_from_slice(&block);
        }
    }
    Bytes::from(frame)
}

/// A microphone that sings a phase-continuous sine of the asked frequency.
#[derive(Debug, Default)]
pub struct VirtualMicrophone {
    streams: Arc<AtomicU64>,
}

impl VirtualMicrophone {
    /// How many streams are open right now.
    pub fn active_streams(&self) -> u64 {
        self.streams.load(Ordering::SeqCst)
    }
}

impl MicrophoneBackend for VirtualMicrophone {
    fn list(&self) -> Result<Vec<super::device::backend::MicrophoneInfo>, AlefError> {
        Ok(vec![super::device::backend::MicrophoneInfo {
            id: VIRTUAL_MICROPHONE.to_owned(),
            label: "Alef virtual microphone".to_owned(),
            rate: VIRTUAL_RATE,
            frequency: 440.0,
        }])
    }

    fn open(
        &self,
        id: &str,
        frequency: f64,
        rate: u32,
    ) -> Result<(Arc<DeviceStream<PcmChunk>>, u32), AlefError> {
        if id != VIRTUAL_MICROPHONE {
            return Err(AlefError::new(
                ErrorCode::NotFound,
                format!("unknown microphone {id}"),
            ));
        }
        if !frequency.is_finite() || !(1.0..=20000.0).contains(&frequency) {
            return Err(substituted_invalid(
                "frequency must be a number between 1 and 20000",
            ));
        }
        if !(8000..=192000).contains(&rate) {
            return Err(substituted_invalid("rate must be between 8000 and 192000"));
        }
        let samples = rate as usize * CHUNK_MS as usize / 1000;
        let streams = self.streams.clone();
        streams.fetch_add(1, Ordering::SeqCst);
        // The exit callback runs on every path, a refused admission included (see the camera).
        let started = DeviceStream::start(
            CHUNK_QUEUE,
            Duration::from_millis(CHUNK_MS),
            move || {
                streams.fetch_sub(1, Ordering::SeqCst);
            },
            move |seq, queue| {
                let mut payload = Vec::with_capacity(samples * 4);
                for step in 0..samples {
                    let phase = seq as f64 * samples as f64 + step as f64;
                    let value = (TAU * frequency * phase / f64::from(rate)).sin() as f32;
                    let mut slot = [0u8; 4];
                    slot.copy_from_slice(&value.to_le_bytes());
                    payload.extend_from_slice(&slot);
                }
                PcmChunk {
                    seq,
                    timestamp_ms: super::device::queue::unix_ms(),
                    dropped: queue.drops(),
                    rate,
                    samples,
                    payload: Bytes::from(payload),
                }
            },
        );
        started.map(|stream| (stream, rate))
    }
}

/// An audio sink that counts what it received.
#[derive(Debug, Default)]
pub struct VirtualAudio;

impl AudioBackend for VirtualAudio {
    fn open(&self) -> Result<AudioSink, AlefError> {
        Ok(AudioSink::new())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Instant;

    const WAIT: Duration = Duration::from_secs(5);

    #[tokio::test]
    async fn the_virtual_camera_paints_full_frames_with_increasing_seq_and_timestamps() {
        let camera = VirtualCamera::default();
        let stream = camera.open(VIRTUAL_CAMERA, 120.0).unwrap();
        let queue = stream.queue();
        let mut previous = None;
        for seq in 1..=5 {
            let frame = tokio::time::timeout(WAIT, queue.pop())
                .await
                .expect("frames must keep coming")
                .expect("the queue must not close");
            assert_eq!(frame.seq, seq);
            if let Some(last) = previous {
                assert!(frame.timestamp_ms > last);
            }
            previous = Some(frame.timestamp_ms);
            assert_eq!(frame.width, VIRTUAL_WIDTH);
            assert_eq!(frame.height, VIRTUAL_HEIGHT);
            assert_eq!(
                frame.payload.len() as u32,
                VIRTUAL_WIDTH * VIRTUAL_HEIGHT * 4
            );
            assert_eq!(frame.dropped, 0);
        }
        stream.stop();
    }

    #[tokio::test]
    async fn the_virtual_camera_never_produces_faster_than_requested() {
        let camera = VirtualCamera::default();
        let stream = camera.open(VIRTUAL_CAMERA, 30.0).unwrap();
        let queue = stream.queue();
        let started = Instant::now();
        for _ in 0..6 {
            tokio::time::timeout(WAIT, queue.pop())
                .await
                .expect("frames must keep coming")
                .expect("the queue must not close");
        }
        assert!(started.elapsed() >= Duration::from_millis(140));
        stream.stop();
    }

    #[tokio::test]
    async fn a_slow_consumer_drops_the_oldest_frames_and_the_headers_report_it() {
        let camera = VirtualCamera::default();
        let stream = camera.open(VIRTUAL_CAMERA, 120.0).unwrap();
        let queue = stream.queue();
        tokio::time::sleep(Duration::from_millis(400)).await;
        let frame = tokio::time::timeout(WAIT, queue.pop())
            .await
            .expect("a frame must come")
            .expect("the queue must not close");
        assert!(frame.seq > 1);
        assert!(frame.dropped > 0);
        stream.stop();
    }

    #[tokio::test]
    async fn the_virtual_microphone_carries_a_sine_of_the_requested_frequency() {
        let microphone = VirtualMicrophone::default();
        let (stream, rate) = microphone.open(VIRTUAL_MICROPHONE, 440.0, 48000).unwrap();
        assert_eq!(rate, VIRTUAL_RATE);
        let queue = stream.queue();
        let mut samples: Vec<f32> = Vec::new();
        while samples.len() < 48000 {
            let chunk = tokio::time::timeout(WAIT, queue.pop())
                .await
                .expect("chunks must keep coming")
                .expect("the queue must not close");
            for value in chunk.payload.chunks_exact(4) {
                let mut slot = [0u8; 4];
                slot.copy_from_slice(value);
                samples.push(f32::from_le_bytes(slot));
            }
        }
        stream.stop();
        let mut crossings = 0usize;
        for pair in samples.windows(2) {
            if (pair[0] > 0.0) != (pair[1] > 0.0) {
                crossings += 1;
            }
        }
        let total = samples.len() as f64;
        let frequency = crossings as f64 * f64::from(VIRTUAL_RATE) / (2.0 * total);
        assert!((frequency - 440.0).abs() < 2.0, "{frequency} Hz");
    }

    #[tokio::test]
    async fn the_virtual_camera_counts_its_live_producers_and_stops_them() {
        let camera = VirtualCamera::default();
        let first = camera.open(VIRTUAL_CAMERA, 30.0).unwrap();
        let second = camera.open(VIRTUAL_CAMERA, 60.0).unwrap();
        assert_eq!(camera.active_streams(), 2);
        first.stop();
        second.stop();
        for join in [first.take_job(), second.take_job()].into_iter().flatten() {
            tokio::time::timeout(WAIT, join.result())
                .await
                .expect("the jobs must stop soon")
                .expect("the jobs must not fail");
        }
        assert_eq!(camera.active_streams(), 0);
    }

    #[tokio::test]
    async fn two_consecutive_virtual_frames_differ_at_the_expected_pixels() {
        let camera = VirtualCamera::default();
        let stream = camera.open(VIRTUAL_CAMERA, 120.0).unwrap();
        let queue = stream.queue();
        let first = tokio::time::timeout(WAIT, queue.pop())
            .await
            .expect("frames must keep coming")
            .expect("the queue must not close");
        let second = tokio::time::timeout(WAIT, queue.pop())
            .await
            .expect("frames must keep coming")
            .expect("the queue must not close");
        let differs =
            |a: &[u8], b: &[u8]| a.len() == b.len() && a.iter().zip(b.iter()).any(|(x, y)| x != y);
        assert!(differs(&first.payload, &second.payload));
        assert_eq!(second.payload[0], first.payload[0].wrapping_add(3));
        assert_eq!(second.payload[1], first.payload[1].wrapping_add(5));
        assert_eq!(second.payload[2], first.payload[2].wrapping_add(11));
        assert_eq!(first.payload[3], 255);
        assert_eq!(second.payload[3], 255);
        let zero = vec![0u8; first.payload.len()];
        assert!(!differs(&zero, &zero));
        stream.stop();
    }
}
