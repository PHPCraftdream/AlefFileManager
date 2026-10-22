// SPDX-License-Identifier: MIT OR Apache-2.0
//! Injectable time and randomness seams, plus the deterministic backoff curve.

use core::time::Duration;
use std::time::Instant;

/// Monotonic clock as a `Duration` from an arbitrary epoch, so fakes stay trivial.
pub trait Clock {
    fn now(&self) -> Duration;
}

/// Real clock on `std::time::Instant`; the epoch is the construction moment.
pub struct SystemClock {
    start: Instant,
}

impl SystemClock {
    pub fn new() -> Self {
        SystemClock {
            start: Instant::now(),
        }
    }
}

impl Default for SystemClock {
    fn default() -> Self {
        Self::new()
    }
}

impl Clock for SystemClock {
    fn now(&self) -> Duration {
        self.start.elapsed()
    }
}

/// Deterministic source of fractions in `[0, 1)`; the wiring provides the
/// implementation, tests provide a fixed fake.
pub trait Jitter {
    fn fraction(&mut self) -> f64;
}

/// Exponential backoff `base * 2^failures`, capped at `max`. Pure and total.
pub(crate) fn capped_delay(base: Duration, max: Duration, failures: u32) -> Duration {
    let base_ms = base.as_millis().max(1);
    let max_ms = max.as_millis();
    let mut ms = base_ms;
    for _ in 0..failures.min(32) {
        ms = ms.saturating_mul(2);
        if ms >= max_ms {
            return max;
        }
    }
    Duration::from_millis(ms.min(max_ms) as u64)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn doubles_then_caps() {
        let base = Duration::from_millis(100);
        let max = Duration::from_millis(1000);
        assert_eq!(capped_delay(base, max, 0), Duration::from_millis(100));
        assert_eq!(capped_delay(base, max, 1), Duration::from_millis(200));
        assert_eq!(capped_delay(base, max, 3), Duration::from_millis(800));
        assert_eq!(capped_delay(base, max, 4), max);
        assert_eq!(capped_delay(base, max, 40), max);
    }

    #[test]
    fn zero_base_stays_positive_and_capped() {
        let max = Duration::from_millis(50);
        assert_eq!(
            capped_delay(Duration::ZERO, max, 0),
            Duration::from_millis(1)
        );
    }
}
