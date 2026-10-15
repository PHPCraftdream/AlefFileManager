// SPDX-License-Identifier: MIT OR Apache-2.0
//! Test/e2e-only detection of synchronous work performed during a future poll.

use std::{
    future::{poll_fn, Future},
    time::{Duration, Instant},
};

const STALL_THRESHOLD: Duration = Duration::from_millis(50);

pub async fn watch<F: Future>(kind: &str, future: F) -> F::Output {
    poll_watch(kind, future, Instant::now, emit).await
}

async fn poll_watch<F: Future, N, E>(kind: &str, future: F, mut now: N, mut report: E) -> F::Output
where
    N: FnMut() -> Instant,
    E: FnMut(&str),
{
    let mut future = Box::pin(future);
    poll_fn(move |cx| {
        let start = now();
        let result = future.as_mut().poll(cx);
        let elapsed = now().saturating_duration_since(start);
        if elapsed > STALL_THRESHOLD {
            report(&format!("ALEF_BUS_STALL {} {}", kind, elapsed.as_millis()));
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
            |line| lines.borrow_mut().push(line.to_owned()),
        ));
        let waker = std::task::Waker::noop();
        let mut cx = Context::from_waker(waker);
        assert!(output.as_mut().poll(&mut cx).is_pending());
        drop(output);
        assert_eq!(*lines.borrow(), ["ALEF_BUS_STALL call/e2e.blockBus 51"]);
    }
}
