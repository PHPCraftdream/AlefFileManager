// SPDX-License-Identifier: MIT OR Apache-2.0
//! Pure audio sample conversions and the bounded PCM ring the system backends share. No
//! hardware, no allocation beyond the returned buffers.
use std::collections::VecDeque;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Mutex;

/// A bounded `f32` ring a page sink fills and the system backend drains. Writes drop the oldest
/// samples when full and count them; reads zero-fill a deficit and count one underrun per such
/// read.
pub struct PcmRing {
    samples: Mutex<VecDeque<f32>>,
    capacity: usize,
    overrun_samples: AtomicU64,
    underruns: AtomicU64,
}

impl PcmRing {
    /// A ring holding at most `capacity_samples`.
    pub fn new(capacity_samples: usize) -> Self {
        assert!(capacity_samples > 0, "the ring capacity must be positive");
        Self {
            samples: Mutex::new(VecDeque::with_capacity(capacity_samples)),
            capacity: capacity_samples,
            overrun_samples: AtomicU64::new(0),
            underruns: AtomicU64::new(0),
        }
    }
    /// Pushes samples, dropping and counting the oldest samples that no longer fit.
    pub fn write(&self, samples: &[f32]) {
        let mut ring = self.samples.lock().unwrap_or_else(|e| e.into_inner());
        self.drain_locked(&mut ring, samples);
    }
    /// Copies buffered samples and zero-fills a deficit; a short read counts one underrun.
    pub fn read(&self, out: &mut [f32]) {
        let mut ring = self.samples.lock().unwrap_or_else(|e| e.into_inner());
        self.fill_locked(&mut ring, out);
    }
    /// Non-blocking callback write; contention drops the chunk.
    pub fn try_write(&self, samples: &[f32]) -> bool {
        let Ok(mut ring) = self.samples.try_lock() else {
            return false;
        };
        self.drain_locked(&mut ring, samples);
        true
    }
    /// Non-blocking callback read; contention zero-fills and counts an underrun.
    pub fn try_read(&self, out: &mut [f32]) -> bool {
        let Ok(mut ring) = self.samples.try_lock() else {
            out.fill(0.0);
            self.underruns.fetch_add(1, Ordering::Relaxed);
            return false;
        };
        self.fill_locked(&mut ring, out);
        true
    }
    fn drain_locked(&self, ring: &mut VecDeque<f32>, samples: &[f32]) {
        let excess = ring.len().saturating_add(samples.len())
            - self.capacity.min(ring.len() + samples.len());
        if samples.len() > self.capacity {
            self.overrun_samples.fetch_add(
                (ring.len() + samples.len() - self.capacity) as u64,
                Ordering::Relaxed,
            );
            ring.clear();
            ring.extend(&samples[samples.len() - self.capacity..]);
        } else {
            for _ in 0..excess {
                ring.pop_front();
            }
            if excess > 0 {
                self.overrun_samples
                    .fetch_add(excess as u64, Ordering::Relaxed);
            }
            ring.extend(samples.iter().copied());
        }
    }
    fn fill_locked(&self, ring: &mut VecDeque<f32>, out: &mut [f32]) {
        let available = ring.len().min(out.len());
        for slot in out.iter_mut().take(available) {
            *slot = ring.pop_front().unwrap_or(0.0);
        }
        for slot in out.iter_mut().skip(available) {
            *slot = 0.0;
        }
        if available < out.len() {
            self.underruns.fetch_add(1, Ordering::Relaxed);
        }
    }
    /// The samples currently buffered.
    pub fn len(&self) -> usize {
        self.samples.lock().unwrap_or_else(|e| e.into_inner()).len()
    }
    /// Whether no sample is buffered.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// Samples dropped for a full ring so far.
    pub fn overruns(&self) -> u64 {
        self.overrun_samples.load(Ordering::Relaxed)
    }
    /// Reads that had to zero-fill so far.
    pub fn underruns(&self) -> u64 {
        self.underruns.load(Ordering::Relaxed)
    }
    /// Test-only lock access for deterministic contention tests.
    #[cfg(test)]
    pub fn lock_for_test(&self) -> std::sync::MutexGuard<'_, VecDeque<f32>> {
        self.samples.lock().unwrap_or_else(|e| e.into_inner())
    }
}

/// Exact rational chunk schedule `floor((n+1)rms/1000)-floor(nrms/1000)`.
pub fn chunk_samples(n: u64, rate: u32, chunk_ms: u64) -> u64 {
    let product = u128::from(rate) * u128::from(chunk_ms);
    let start = u128::from(n) * product / 1000;
    let end = (u128::from(n) + 1) * product / 1000;
    u64::try_from(end - start).unwrap_or(u64::MAX)
}

pub fn f32_from_i16_one(v: i16) -> f32 {
    f32::from(v) / 32768.0
}
pub fn f32_from_u16_one(v: u16) -> f32 {
    (f32::from(v) - 32768.0) / 32768.0
}
/// f32 loses precision for many i32 values; exact round-trip is limited to representable samples.
pub fn f32_from_i32_one(v: i32) -> f32 {
    v as f32 / 2147483648.0
}
pub fn f32_from_i16(src: &[i16]) -> Vec<f32> {
    src.iter().map(|&v| f32_from_i16_one(v)).collect()
}
pub fn f32_from_u16(src: &[u16]) -> Vec<f32> {
    src.iter().map(|&v| f32_from_u16_one(v)).collect()
}
pub fn f32_from_i32(src: &[i32]) -> Vec<f32> {
    src.iter().map(|&v| f32_from_i32_one(v)).collect()
}
/// Symmetrically scales [-1, 1] to signed PCM, saturates endpoints, and maps NaN to silence (0).
pub fn i16_from_f32_one(v: f32) -> i16 {
    let v = if v.is_nan() { 0.0 } else { v };
    (f64::from(v.clamp(-1.0, 1.0)) * 32768.0)
        .round()
        .clamp(f64::from(i16::MIN), f64::from(i16::MAX)) as i16
}
/// Symmetrically scales [-1, 1] to unsigned PCM centered at silence (32768), saturates endpoints,
/// and maps NaN to silence.
pub fn u16_from_f32_one(v: f32) -> u16 {
    let v = if v.is_nan() { 0.0 } else { v };
    (f64::from(v.clamp(-1.0, 1.0)) * 32768.0 + 32768.0)
        .round()
        .clamp(0.0, f64::from(u16::MAX)) as u16
}
/// Symmetrically scales [-1, 1] to signed 32-bit PCM, saturates endpoints, and maps NaN to silence.
pub fn i32_from_f32_one(v: f32) -> i32 {
    let v = if v.is_nan() { 0.0 } else { v };
    (f64::from(v.clamp(-1.0, 1.0)) * 2147483648.0)
        .round()
        .clamp(f64::from(i32::MIN), f64::from(i32::MAX)) as i32
}
pub fn i16_from_f32(src: &[f32]) -> Vec<i16> {
    src.iter().map(|&v| i16_from_f32_one(v)).collect()
}
pub fn u16_from_f32(src: &[f32]) -> Vec<u16> {
    src.iter().map(|&v| u16_from_f32_one(v)).collect()
}
pub fn i32_from_f32(src: &[f32]) -> Vec<i32> {
    src.iter().map(|&v| i32_from_f32_one(v)).collect()
}

/// Allocation-free downmix; ignores trailing partial frames and returns frames written.
pub fn downmix_into<T: Copy>(
    src: &[T],
    channels: usize,
    dest: &mut [f32],
    convert: fn(T) -> f32,
) -> usize {
    if channels == 0 {
        return 0;
    }
    let frames = (src.len() / channels).min(dest.len());
    for (frame, out) in src.chunks_exact(channels).zip(dest.iter_mut()).take(frames) {
        *out = frame.iter().copied().map(convert).sum::<f32>() / channels as f32;
    }
    frames
}
/// Allocation-free mono replication; returns samples written.
pub fn upmix_into<T: Copy>(
    mono: &[f32],
    channels: usize,
    dest: &mut [T],
    encode: fn(f32) -> T,
) -> usize {
    if channels == 0 {
        return 0;
    }
    let frames = mono.len().min(dest.len() / channels);
    for (sample, frame) in mono
        .iter()
        .copied()
        .zip(dest.chunks_exact_mut(channels))
        .take(frames)
    {
        frame.fill(encode(sample));
    }
    frames * channels
}
pub fn to_mono_interleaved(src: &[f32], channels: usize) -> Vec<f32> {
    if channels <= 1 {
        return src.to_vec();
    }
    let mut out = vec![0.0; src.len() / channels];
    downmix_into(src, channels, &mut out, |v| v);
    out
}
pub fn from_mono(mono: &[f32], channels: usize) -> Vec<f32> {
    let mut out = vec![0.0; mono.len().saturating_mul(channels)];
    upmix_into(mono, channels, &mut out, |v| v);
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::mpsc;
    use std::sync::Arc;
    use std::time::Duration;

    #[test]
    fn ring_drops_oldest_and_counts_overrun() {
        let ring = PcmRing::new(3);
        ring.write(&[1.0, 2.0, 3.0]);
        ring.write(&[4.0]);
        let mut out = [0.0; 3];
        ring.read(&mut out);
        assert_eq!(out, [2.0, 3.0, 4.0]);
        assert_eq!(ring.overruns(), 1);
    }
    #[test]
    fn ring_keeps_tail_of_write_larger_than_capacity() {
        let ring = PcmRing::new(2);
        ring.write(&[1.0, 2.0, 3.0, 4.0]);
        let mut out = [0.0; 2];
        ring.read(&mut out);
        assert_eq!(out, [3.0, 4.0]);
        assert_eq!(ring.overruns(), 2);
    }
    #[test]
    fn ring_zero_fills_short_read_and_counts_underrun() {
        let ring = PcmRing::new(3);
        ring.write(&[1.0]);
        let mut out = [9.0; 3];
        ring.read(&mut out);
        assert_eq!(out, [1.0, 0.0, 0.0]);
        assert_eq!(ring.underruns(), 1);
    }

    #[test]
    fn contention_try_write_returns_without_deadlock() {
        let ring = Arc::new(PcmRing::new(4));
        ring.write(&[1.0]);
        let guard = ring.lock_for_test();
        assert_eq!(*guard, VecDeque::from([1.0]));
        let (tx, rx) = mpsc::channel();
        let worker_ring = Arc::clone(&ring);
        let worker = std::thread::spawn(move || tx.send(worker_ring.try_write(&[2.0])).unwrap());
        let result = rx.recv_timeout(Duration::from_secs(2));
        drop(guard);
        assert_eq!(result, Ok(false));
        worker.join().expect("worker completes after guard release");
        assert_eq!(ring.len(), 1);
        assert_eq!(ring.overruns(), 0);
    }
    #[test]
    fn contention_try_read_returns_silence_without_deadlock() {
        let ring = Arc::new(PcmRing::new(4));
        ring.write(&[1.0]);
        let guard = ring.lock_for_test();
        assert_eq!(*guard, VecDeque::from([1.0]));
        let (tx, rx) = mpsc::channel();
        let worker_ring = Arc::clone(&ring);
        let worker = std::thread::spawn(move || {
            let mut out = [7.5; 3];
            let result = worker_ring.try_read(&mut out);
            tx.send((result, out)).unwrap();
        });
        let result = rx.recv_timeout(Duration::from_secs(2));
        drop(guard);
        assert_eq!(result, Ok((false, [0.0; 3])));
        worker.join().expect("worker completes after guard release");
        assert_eq!(ring.len(), 1);
        assert_eq!(ring.underruns(), 1);
    }
    #[test]
    fn rational_schedule_has_no_drift_at_44100_and_44101() {
        for rate in [44100, 44101] {
            let total: u64 = (0..50).map(|n| chunk_samples(n, rate, 20)).sum();
            assert_eq!(total, u64::from(rate));
        }
        assert_eq!(chunk_samples(0, 44101, 20), 882);
    }
    #[test]
    fn pcm_decode_and_encode_use_independent_endpoints_and_midpoint_oracles() {
        assert_eq!(
            f32_from_i16(&[i16::MIN, 0, i16::MAX]),
            [-1.0, 0.0, 32767.0 / 32768.0]
        );
        assert_eq!(
            f32_from_u16(&[0, 32768, u16::MAX]),
            [-1.0, 0.0, 32767.0 / 32768.0]
        );
        assert_eq!(f32_from_i32(&[i32::MIN, 0, i32::MAX]), [-1.0, 0.0, 1.0]);
        assert_eq!(
            i16_from_f32(&[-2.0, -0.5, 0.5, 2.0, f32::NAN]),
            [i16::MIN, -16384, 16384, i16::MAX, 0]
        );
        assert_eq!(
            u16_from_f32(&[-2.0, -1.0, 0.0, 1.0, 2.0, f32::NAN]),
            [0, 0, 32768, u16::MAX, u16::MAX, 32768]
        );
        assert_eq!(
            i32_from_f32(&[-2.0, -1.0, 0.0, 1.0, 2.0, f32::NAN]),
            [i32::MIN, i32::MIN, 0, i32::MAX, i32::MAX, 0]
        );
    }
    #[test]
    fn signed_and_unsigned_pcm_endpoints_round_trip_and_nan_is_silence() {
        let signed16 = [i16::MIN, -16_384, 0, 16_384, i16::MAX];
        assert_eq!(i16_from_f32(&f32_from_i16(&signed16)), signed16);
        let unsigned16 = [0, 1, 32_768, 65_534, u16::MAX];
        assert_eq!(u16_from_f32(&f32_from_u16(&unsigned16)), unsigned16);
        assert_eq!(u16_from_f32_one(f32::NAN), 32768);
        assert_eq!(i16_from_f32_one(f32::NAN), 0);
        assert_eq!(i32_from_f32_one(f32::NAN), 0);
        let signed32 = [i32::MIN, -1_073_741_824, 0, 1_073_741_824, i32::MAX];
        assert_eq!(
            i32_from_f32(&f32_from_i32(&signed32)),
            [i32::MIN, -1_073_741_824, 0, 1_073_741_824, i32::MAX]
        );
    }
    #[test]
    fn allocation_free_callback_helpers_write_expected_frames() {
        let mut mono = [0.0; 2];
        assert_eq!(
            downmix_into(&[0.0, 1.0, 0.5, -0.5, 9.0], 2, &mut mono, |v| v),
            2
        );
        assert_eq!(mono, [0.5, 0.0]);
        let mut stereo = [0i16; 5];
        assert_eq!(
            upmix_into(&[0.5, -0.5], 2, &mut stereo, i16_from_f32_one),
            4
        );
        assert_eq!(stereo, [16384, 16384, -16384, -16384, 0]);
    }
}
