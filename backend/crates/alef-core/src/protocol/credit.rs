// SPDX-License-Identifier: MIT OR Apache-2.0
//! Credit-based stream flow control and byte chunking.
use bytes::Bytes;
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Mutex,
};
use tokio::sync::Notify;

/// Default stream window (1 MiB).
pub const DEFAULT_STREAM_WINDOW: usize = 1024 * 1024;
/// Default send chunk size (256 KiB).
pub const DEFAULT_CHUNK_SIZE: usize = 256 * 1024;

/// Why [`CreditGate::acquire`] failed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AcquireError {
    /// Gate was closed.
    Closed,
    /// Request larger than the whole window; caller must split.
    TooLarge { requested: usize, window: usize },
}

/// Credit gate; acquiring waits, grants saturate, and close wakes all waiters.
pub struct CreditGate {
    window: usize,
    outstanding: Mutex<usize>,
    notify: Notify,
    closed: AtomicBool,
}
impl CreditGate {
    /// Creates a gate with the default window.
    pub fn new() -> Self {
        Self::with_window(DEFAULT_STREAM_WINDOW)
    }
    /// Creates a gate with a nonzero window.
    pub fn with_window(window: usize) -> Self {
        assert!(window > 0, "window must be nonzero");
        Self {
            window,
            outstanding: Mutex::new(0),
            notify: Notify::new(),
            closed: AtomicBool::new(false),
        }
    }
    /// Returns the gate window.
    pub fn window(&self) -> usize {
        self.window
    }
    /// Waits until acquiring `n` preserves the window invariant.
    pub async fn acquire(&self, n: usize) -> Result<(), AcquireError> {
        if n > self.window {
            return Err(AcquireError::TooLarge {
                requested: n,
                window: self.window,
            });
        }
        loop {
            let mut notified = std::pin::pin!(self.notify.notified());
            notified.as_mut().enable();
            // checked after `enable`: a close racing before it would otherwise be a lost wakeup
            if self.closed.load(Ordering::Acquire) {
                return Err(AcquireError::Closed);
            }
            {
                let mut state = self.outstanding.lock().expect("credit mutex poisoned");
                if n <= self.window - *state {
                    *state += n;
                    return Ok(());
                }
            }
            notified.await;
        }
    }
    /// Returns credit, saturating outstanding bytes at zero.
    pub fn grant(&self, n: usize) {
        let mut state = self.outstanding.lock().expect("credit mutex poisoned");
        *state = state.saturating_sub(n);
        drop(state);
        self.notify.notify_waiters();
    }
    /// Closes the gate and wakes all waiters.
    pub fn close(&self) {
        self.closed.store(true, Ordering::Release);
        self.notify.notify_waiters();
    }
    /// Reports whether the gate is closed.
    pub fn is_closed(&self) -> bool {
        self.closed.load(Ordering::Acquire)
    }
    /// Returns outstanding acquired bytes.
    pub fn outstanding(&self) -> usize {
        *self.outstanding.lock().expect("credit mutex poisoned")
    }
}
impl Default for CreditGate {
    fn default() -> Self {
        Self::new()
    }
}

/// Iterator over ordered pieces no larger than the configured maximum.
pub struct ChunkIter {
    bytes: Bytes,
    max: usize,
}
impl Iterator for ChunkIter {
    type Item = Bytes;
    fn next(&mut self) -> Option<Bytes> {
        if self.bytes.is_empty() {
            None
        } else {
            let n = self.max.min(self.bytes.len());
            Some(self.bytes.split_to(n))
        }
    }
}
/// Splits bytes into pieces no larger than `max`; panics if `max` is zero.
pub fn chunk(bytes: Bytes, max: usize) -> ChunkIter {
    assert!(max > 0, "chunk maximum must be nonzero");
    ChunkIter { bytes, max }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::time::timeout;
    #[tokio::test]
    async fn credit_accounting_and_large_requests() {
        let g = CreditGate::with_window(4);
        timeout(Duration::from_secs(10), g.acquire(4))
            .await
            .expect("must not time out")
            .expect("acquire");
        assert_eq!(g.outstanding(), 4);
        g.grant(9);
        assert_eq!(g.outstanding(), 0);
        assert_eq!(
            timeout(Duration::from_secs(10), g.acquire(5))
                .await
                .expect("must not time out"),
            Err(AcquireError::TooLarge {
                requested: 5,
                window: 4
            })
        );
    }
    #[tokio::test]
    async fn waits_until_credit_and_closes() {
        let g = std::sync::Arc::new(CreditGate::with_window(4));
        timeout(Duration::from_secs(10), g.acquire(4))
            .await
            .expect("must not time out")
            .expect("fill");
        let task = {
            let g = g.clone();
            tokio::spawn(async move { g.acquire(1).await })
        };
        timeout(Duration::from_secs(10), tokio::task::yield_now())
            .await
            .expect("must not time out");
        assert!(!task.is_finished());
        g.grant(1);
        assert_eq!(
            timeout(Duration::from_secs(10), task)
                .await
                .expect("must not time out")
                .expect("join"),
            Ok(())
        );
        g.close();
        assert!(g.is_closed());
        assert_eq!(
            timeout(Duration::from_secs(10), g.acquire(1))
                .await
                .expect("must not time out"),
            Err(AcquireError::Closed)
        );
        g.grant(10);
        assert_eq!(g.outstanding(), 0);
    }
    #[test]
    fn default_window_matches_constant() {
        assert_eq!(CreditGate::new().window(), DEFAULT_STREAM_WINDOW);
    }

    #[tokio::test]
    async fn blocked_acquirer_resumes_after_grant() {
        let g = std::sync::Arc::new(CreditGate::with_window(4));
        timeout(Duration::from_secs(10), g.acquire(4))
            .await
            .expect("must not time out")
            .expect("fill");
        let task = {
            let g = g.clone();
            tokio::spawn(async move { g.acquire(1).await })
        };
        timeout(Duration::from_secs(10), tokio::task::yield_now())
            .await
            .expect("must not time out");
        assert!(
            timeout(Duration::from_millis(100), g.acquire(1))
                .await
                .is_err(),
            "must stay blocked while the window is exhausted"
        );
        g.grant(1);
        assert_eq!(
            timeout(Duration::from_secs(10), task)
                .await
                .expect("must not time out")
                .expect("join"),
            Ok(())
        );
        assert_eq!(g.outstanding(), 4);
    }

    #[tokio::test]
    async fn close_releases_blocked_acquirer() {
        let g = std::sync::Arc::new(CreditGate::with_window(4));
        timeout(Duration::from_secs(10), g.acquire(4))
            .await
            .expect("must not time out")
            .expect("fill");
        let mut task = {
            let g = g.clone();
            tokio::spawn(async move { g.acquire(1).await })
        };
        timeout(Duration::from_secs(10), tokio::task::yield_now())
            .await
            .expect("must not time out");
        assert!(
            timeout(Duration::from_millis(100), &mut task)
                .await
                .is_err(),
            "must stay blocked while the window is exhausted"
        );
        g.close();
        assert_eq!(
            timeout(Duration::from_secs(10), task)
                .await
                .expect("must not time out")
                .expect("join"),
            Err(AcquireError::Closed)
        );
        assert_eq!(g.outstanding(), 4);
        assert!(g.is_closed());
    }

    #[tokio::test]
    async fn concurrent_producers_never_exceed_window() {
        let g = std::sync::Arc::new(CreditGate::with_window(1024));
        let (tx, mut rx) = tokio::sync::mpsc::unbounded_channel();
        for _ in 0..8 {
            let g = g.clone();
            let tx = tx.clone();
            tokio::spawn(async move {
                for _ in 0..16 {
                    g.acquire(64).await.expect("credit");
                    tx.send(64u32).expect("receiver");
                }
            });
        }
        drop(tx);
        let mut max = 0;
        let mut total = 0;
        while let Some(n) = timeout(Duration::from_secs(10), rx.recv())
            .await
            .expect("must not time out")
        {
            max = max.max(g.outstanding());
            total += n;
            g.grant(64);
        }
        assert_eq!(total, 8192);
        assert!(max <= 1024);
    }
    #[test]
    fn chunk_preserves_order_and_boundaries() {
        let input = Bytes::from((0..35).map(|i| i as u8).collect::<Vec<_>>());
        let pieces: Vec<_> = chunk(input.clone(), 10).collect();
        assert_eq!(
            pieces.iter().map(Bytes::len).collect::<Vec<_>>(),
            vec![10, 10, 10, 5]
        );
        let joined: Vec<_> = pieces.iter().flat_map(|b| b.iter().copied()).collect();
        assert_eq!(joined, input);
        assert!(chunk(Bytes::new(), 10).next().is_none());
    }
    #[test]
    #[should_panic]
    fn chunk_rejects_zero_maximum() {
        let _ = chunk(Bytes::new(), 0);
    }

    /// Probabilistic by nature (real threads): a close must never be lost to a waiter that is
    /// between its closed-check and its registration.
    #[test]
    fn close_racing_a_new_waiter_is_never_lost() {
        for round in 0..300u64 {
            let g = std::sync::Arc::new(CreditGate::with_window(1));
            let rt = tokio::runtime::Builder::new_current_thread()
                .enable_time()
                .build()
                .expect("runtime");
            rt.block_on(g.acquire(1)).expect("exhaust the window");
            let waiter = {
                let g = g.clone();
                std::thread::spawn(move || {
                    let rt = tokio::runtime::Builder::new_current_thread()
                        .enable_time()
                        .build()
                        .expect("runtime");
                    rt.block_on(async {
                        timeout(Duration::from_secs(10), g.acquire(1))
                            .await
                            .expect("close must wake the waiter")
                    })
                })
            };
            std::thread::sleep(Duration::from_micros(round % 40 * 5));
            g.close();
            assert_eq!(waiter.join().expect("waiter"), Err(AcquireError::Closed));
        }
    }
}
