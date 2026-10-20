// SPDX-License-Identifier: MIT OR Apache-2.0
//! Test/e2e-only detection of synchronous work performed during a future poll.
//!
//! A poll that takes longer than the threshold is reported as `ALEF_BUS_STALL <kind> <ms>`. The
//! wall clock cannot tell a blocked bus from a machine that stopped scheduling the whole process
//! (a paused or throttled virtual machine): a heartbeat thread notes every time it woke far later
//! than it slept, and a stall that those pauses mostly explain is reported as
//! `ALEF_BUS_STALL_STARVED <kind> <ms> <unscheduled_ms>`, which is not a bus defect.

use std::{
    collections::VecDeque,
    future::{poll_fn, Future},
    sync::{Mutex, Once},
    thread,
    time::{Duration, Instant},
};

const STALL_THRESHOLD: Duration = Duration::from_millis(50);
/// How long the heartbeat thread sleeps between looks at the clock.
const BEAT: Duration = Duration::from_millis(10);
/// A heartbeat that woke this much later than it should have means the process was not scheduled.
const LATE: Duration = Duration::from_millis(25);
/// How many recent pauses are kept.
const KEPT: usize = 128;

/// Spans (sleep start, wake) of the heartbeat oversleeps, oldest first.
static PAUSES: Mutex<VecDeque<(Instant, Instant)>> = Mutex::new(VecDeque::new());

fn start_heartbeat() {
    static STARTED: Once = Once::new();
    STARTED.call_once(|| {
        let _ = thread::Builder::new()
            .name("alef-bus-heartbeat".to_owned())
            .spawn(|| loop {
                let before = Instant::now();
                thread::sleep(BEAT);
                let after = Instant::now();
                if after.saturating_duration_since(before) > BEAT + LATE {
                    let mut pauses = PAUSES.lock().unwrap_or_else(|e| e.into_inner());
                    if pauses.len() == KEPT {
                        pauses.pop_front();
                    }
                    pauses.push_back((before, after));
                }
            });
    });
}

/// The time of `[start, end]` that lies inside the given pauses.
fn overlap(pauses: &VecDeque<(Instant, Instant)>, start: Instant, end: Instant) -> Duration {
    pauses
        .iter()
        .map(|&(from, to)| to.min(end).saturating_duration_since(from.max(start)))
        .sum()
}

fn unscheduled(start: Instant, end: Instant) -> Duration {
    overlap(
        &PAUSES.lock().unwrap_or_else(|e| e.into_inner()),
        start,
        end,
    )
}

pub async fn watch<F: Future>(kind: &str, future: F) -> F::Output {
    start_heartbeat();
    poll_watch(kind, future, Instant::now, unscheduled, emit).await
}

async fn poll_watch<F: Future, N, P, E>(
    kind: &str,
    future: F,
    mut now: N,
    mut paused: P,
    mut report: E,
) -> F::Output
where
    N: FnMut() -> Instant,
    P: FnMut(Instant, Instant) -> Duration,
    E: FnMut(&str),
{
    let mut future = Box::pin(future);
    poll_fn(move |cx| {
        let start = now();
        let result = future.as_mut().poll(cx);
        let end = now();
        let elapsed = end.saturating_duration_since(start);
        if elapsed > STALL_THRESHOLD {
            let lost = paused(start, end);
            if lost * 2 >= elapsed {
                report(&format!(
                    "ALEF_BUS_STALL_STARVED {} {} {}",
                    kind,
                    elapsed.as_millis(),
                    lost.as_millis()
                ));
            } else {
                report(&format!("ALEF_BUS_STALL {} {}", kind, elapsed.as_millis()));
            }
        }
        result
    })
    .await
}

fn emit(line: &str) {
    eprintln!("{line}");
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        cell::RefCell,
        future::poll_fn,
        sync::atomic::{AtomicUsize, Ordering},
        task::{Context, Poll},
    };

    #[test]
    fn threshold_is_strict_and_result_propagates() {
        for (millis, expected) in [
            (50, None),
            (51, Some("ALEF_BUS_STALL call/e2e.blockBus 51")),
        ] {
            let start = Instant::now();
            let clock_index = AtomicUsize::new(0);
            let lines = RefCell::new(Vec::new());
            let now = || {
                let index = clock_index.fetch_add(1, Ordering::Relaxed);
                start + Duration::from_millis(if index % 2 == 0 { 0 } else { millis })
            };
            let mut output = Box::pin(poll_watch(
                "call/e2e.blockBus",
                poll_fn(|_| Poll::Ready(42)),
                now,
                |_, _| Duration::ZERO,
                |line| lines.borrow_mut().push(line.to_owned()),
            ));
            let waker = std::task::Waker::noop();
            let mut cx = Context::from_waker(waker);
            assert_eq!(output.as_mut().poll(&mut cx), Poll::Ready(42));
            drop(output);
            match expected {
                Some(expected) => assert_eq!(*lines.borrow(), [expected]),
                None => assert!(lines.borrow().is_empty()),
            }
        }
    }

    #[test]
    fn pending_waits_do_not_accumulate_across_polls() {
        let start = Instant::now();
        let ticks = AtomicUsize::new(0);
        let times = [0, 1, 1000, 1001, 2000, 2001, 3000, 3001];
        let clock_index = AtomicUsize::new(0);
        let mut lines = Vec::new();
        let future = poll_fn(|_| {
            let tick = ticks.fetch_add(1, Ordering::Relaxed) + 1;
            if tick < 4 {
                Poll::Pending
            } else {
                Poll::Ready(())
            }
        });
        let output = poll_watch(
            "call/e2e.blockBus",
            future,
            || start + Duration::from_millis(times[clock_index.fetch_add(1, Ordering::Relaxed)]),
            |_, _| Duration::ZERO,
            |line| lines.push(line.to_owned()),
        );
        let waker = std::task::Waker::noop();
        let mut cx = Context::from_waker(waker);
        let mut output = Box::pin(output);
        for _ in 0..4 {
            let ready = output.as_mut().poll(&mut cx).is_ready();
            assert_eq!(ready, ticks.load(Ordering::Relaxed) == 4);
        }
        drop(output);
        assert!(lines.is_empty());
    }

    #[test]
    fn pending_poll_reports_slow_poll() {
        let start = Instant::now();
        let times = [0, 51];
        let clock_index = AtomicUsize::new(0);
        let lines = RefCell::new(Vec::new());
        let mut output = Box::pin(poll_watch(
            "call/e2e.blockBus",
            poll_fn(|_| Poll::Pending::<()>),
            || start + Duration::from_millis(times[clock_index.fetch_add(1, Ordering::Relaxed)]),
            |_, _| Duration::ZERO,
            |line| lines.borrow_mut().push(line.to_owned()),
        ));
        let waker = std::task::Waker::noop();
        let mut cx = Context::from_waker(waker);
        assert!(output.as_mut().poll(&mut cx).is_pending());
        drop(output);
        assert_eq!(*lines.borrow(), ["ALEF_BUS_STALL call/e2e.blockBus 51"]);
    }

    /// One slow poll of `elapsed` ms during which the process was unscheduled for `lost` ms.
    fn report_for(elapsed: u64, lost: u64) -> Vec<String> {
        let start = Instant::now();
        let clock_index = AtomicUsize::new(0);
        let lines = RefCell::new(Vec::new());
        let mut output = Box::pin(poll_watch(
            "call/window.state",
            poll_fn(|_| Poll::Ready(())),
            || {
                start
                    + Duration::from_millis(if clock_index.fetch_add(1, Ordering::Relaxed) == 0 {
                        0
                    } else {
                        elapsed
                    })
            },
            |_, _| Duration::from_millis(lost),
            |line| lines.borrow_mut().push(line.to_owned()),
        ));
        let waker = std::task::Waker::noop();
        let mut cx = Context::from_waker(waker);
        assert!(output.as_mut().poll(&mut cx).is_ready());
        drop(output);
        lines.into_inner()
    }

    #[test]
    fn a_stall_that_pauses_of_the_process_mostly_explain_is_starved_not_a_stall() {
        assert_eq!(
            report_for(135, 135),
            ["ALEF_BUS_STALL_STARVED call/window.state 135 135"]
        );
        assert_eq!(
            report_for(100, 50),
            ["ALEF_BUS_STALL_STARVED call/window.state 100 50"]
        );
    }

    #[test]
    fn a_stall_that_pauses_barely_explain_is_still_a_stall() {
        assert_eq!(report_for(150, 0), ["ALEF_BUS_STALL call/window.state 150"]);
        assert_eq!(
            report_for(150, 74),
            ["ALEF_BUS_STALL call/window.state 150"]
        );
    }

    #[test]
    fn only_the_part_of_a_pause_inside_the_poll_counts() {
        let origin = Instant::now();
        let at = |ms: u64| origin + Duration::from_millis(ms);
        let pauses: VecDeque<_> = [(at(0), at(40)), (at(90), at(130)), (at(300), at(400))].into();
        assert_eq!(
            overlap(&pauses, at(10), at(100)),
            Duration::from_millis(30 + 10)
        );
        assert_eq!(overlap(&pauses, at(40), at(90)), Duration::ZERO);
        assert_eq!(overlap(&pauses, at(0), at(500)), Duration::from_millis(180));
        assert_eq!(overlap(&VecDeque::new(), at(0), at(500)), Duration::ZERO);
    }
}
