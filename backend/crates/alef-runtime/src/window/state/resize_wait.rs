// SPDX-License-Identifier: MIT OR Apache-2.0
// Pure wait/signalling logic for the synchronous resize frame wait.
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Instant;

pub type WakeGeneration = Arc<(Mutex<u64>, Condvar)>;

static TRACE: AtomicBool = AtomicBool::new(false);

pub fn set_resize_trace(enabled: bool) {
    TRACE.store(enabled, Ordering::Relaxed);
}

pub fn resize_trace_enabled() -> bool {
    TRACE.load(Ordering::Relaxed)
}

pub fn new_generation() -> WakeGeneration {
    Arc::new((Mutex::new(0), Condvar::new()))
}

/// Wake signal: bump the generation, then notify waiters.
pub fn bump_generation(gen: &WakeGeneration) {
    let (lock, cond) = &**gen;
    let mut value = lock.lock().unwrap();
    *value = value.wrapping_add(1);
    cond.notify_all();
}

/// Wait until the generation changes from `start`, or `deadline` passes.
/// Returns `true` if the generation advanced, `false` on timeout.
pub fn wait_for_generation(gen: &WakeGeneration, start: u64, deadline: Instant) -> bool {
    let (lock, cond) = &**gen;
    let mut value = lock.lock().unwrap();
    loop {
        if *value != start {
            return true;
        }
        let now = Instant::now();
        if now >= deadline {
            return false;
        }
        let (changed, _) = cond
            .wait_timeout_while(value, deadline - now, |current| *current == start)
            .unwrap();
        value = changed;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    #[test]
    fn returns_when_generation_advances() {
        let gen = new_generation();
        let waiter = gen.clone();
        let start = *gen.0.lock().unwrap();
        let thread = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(20));
            bump_generation(&waiter);
        });
        assert!(wait_for_generation(
            &gen,
            start,
            Instant::now() + Duration::from_secs(5)
        ));
        thread.join().unwrap();
    }

    #[test]
    fn notify_without_increment_times_out() {
        let gen = new_generation();
        let waiter = gen.clone();
        let start = *gen.0.lock().unwrap();
        let thread = std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(10));
            // Signal without bumping: waiters must not treat this as progress.
            waiter.1.notify_all();
        });
        let began = Instant::now();
        assert!(!wait_for_generation(
            &gen,
            start,
            began + Duration::from_millis(500)
        ));
        assert!(began.elapsed() >= Duration::from_millis(100));
        thread.join().unwrap();
    }

    #[test]
    fn already_advanced_returns_immediately() {
        let gen = new_generation();
        let start = *gen.0.lock().unwrap();
        bump_generation(&gen);
        let began = Instant::now();
        assert!(wait_for_generation(
            &gen,
            start,
            began + Duration::from_millis(100)
        ));
        assert!(began.elapsed() < Duration::from_millis(100));
    }
}
