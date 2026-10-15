// SPDX-License-Identifier: MIT OR Apache-2.0
//! Bounded admission for blocking handler work.

use std::num::NonZeroUsize;
use std::panic::{catch_unwind, AssertUnwindSafe};
use std::sync::Arc;

use tokio::sync::{oneshot, Semaphore};

use crate::{AlefError, ErrorCode};

/// Admits at most `max_outstanding` queued or running blocking jobs.
#[derive(Clone)]
pub struct BlockingExecutor {
    admission: Arc<Semaphore>,
}

/// The result of an admitted blocking job. Dropping it cancels queued work when possible; running
/// work cannot be stopped and must cooperate through its call context and perform its own cleanup.
pub struct BlockingJob<R> {
    result: oneshot::Receiver<Result<R, AlefError>>,
}

impl BlockingExecutor {
    /// Creates an executor with a non-zero outstanding-job limit.
    pub fn new(max_outstanding: NonZeroUsize) -> Self {
        assert!(
            max_outstanding.get() <= Semaphore::MAX_PERMITS,
            "blocking job limit exceeds Tokio's semaphore limit"
        );
        Self {
            admission: Arc::new(Semaphore::new(max_outstanding.get())),
        }
    }

    /// Attempts admission without waiting, then schedules `work` on Tokio's blocking pool.
    ///
    /// Requires an active Tokio runtime. The blocking task handle is intentionally detached by
    /// design: the task owns its admission permit and reply sender, so dropping this job cannot
    /// release capacity while running work continues. If its result is no longer wanted, queued
    /// work is skipped and running work should use cooperative cancellation via its call context.
    /// Receiving a job result guarantees that its admission permit has already been released.
    ///
    /// `submit` requires `Send` work and results.
    ///
    /// ```compile_fail
    /// use alef_core::session::executor::BlockingExecutor;
    /// use std::{num::NonZeroUsize, rc::Rc};
    /// let executor = BlockingExecutor::new(NonZeroUsize::new(1).unwrap());
    /// let local = Rc::new(());
    /// let _ = executor.submit(move || drop(local));
    /// ```
    pub fn submit<R, F>(&self, work: F) -> Result<BlockingJob<R>, AlefError>
    where
        R: Send + 'static,
        F: FnOnce() -> R + Send + 'static,
    {
        let permit = Arc::clone(&self.admission)
            .try_acquire_owned()
            .map_err(|_| AlefError::new(ErrorCode::Busy, "blocking executor is full"))?;
        let (sender, result) = oneshot::channel();
        // Intentional detach: the closure owns the permit until work exits, even if its receiver
        // is dropped. The receiver check avoids starting work canceled while still queued.
        let _detached = tokio::task::spawn_blocking(move || {
            let _permit = permit;
            if sender.is_closed() {
                return;
            }
            let outcome = catch_unwind(AssertUnwindSafe(work))
                .map_err(|_| AlefError::new(ErrorCode::Internal, "blocking task panicked"));
            drop(_permit);
            let _ = sender.send(outcome);
        });
        Ok(BlockingJob { result })
    }
}

impl<R> BlockingJob<R> {
    /// Awaits the job's value; a dropped reply sender is reported as `Closed`.
    pub async fn result(self) -> Result<R, AlefError> {
        self.result
            .await
            .map_err(|_| AlefError::new(ErrorCode::Closed, "blocking job was cancelled"))?
    }
}

#[cfg(test)]
mod tests {
    use std::num::NonZeroUsize;
    use std::sync::mpsc;
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    use std::time::Duration;

    use super::*;

    const LIMIT: Duration = Duration::from_secs(5);

    fn executor(capacity: usize) -> BlockingExecutor {
        BlockingExecutor::new(NonZeroUsize::new(capacity).expect("non-zero test limit"))
    }

    async fn result<R>(job: BlockingJob<R>) -> Result<R, AlefError> {
        tokio::time::timeout(LIMIT, job.result())
            .await
            .expect("blocking job completes")
    }

    #[tokio::test]
    async fn submit_runs_work_off_the_calling_thread_and_returns_the_value() {
        let caller = std::thread::current().id();
        let job = executor(1)
            .submit(move || (std::thread::current().id(), 42))
            .expect("admitted");
        let (worker, value) = result(job).await.expect("work succeeds");
        assert_ne!(worker, caller);
        assert_eq!(value, 42);
    }

    #[tokio::test]
    async fn a_panic_maps_to_internal_and_releases_capacity() {
        let executor = executor(1);
        let error = result(executor.submit(|| panic!("work panic")).expect("admitted"))
            .await
            .expect_err("panic is an error");
        assert_eq!(error.code, ErrorCode::Internal);
        let value = result(executor.submit(|| 9).expect("permit released"))
            .await
            .expect("work succeeds");
        assert_eq!(value, 9);
    }

    #[tokio::test]
    async fn a_running_job_holds_capacity_until_it_finishes() {
        let executor = executor(1);
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = mpsc::sync_channel::<()>(1);
        let job = executor
            .submit(move || {
                let _ = started_tx.send(());
                let _ = release_rx.recv_timeout(LIMIT);
            })
            .expect("admitted");
        tokio::time::timeout(LIMIT, started_rx)
            .await
            .expect("worker started")
            .expect("signal");
        assert_eq!(
            executor.submit(|| ()).err().expect("full").code,
            ErrorCode::Busy
        );
        drop(job);
        assert_eq!(
            executor.submit(|| ()).err().expect("still running").code,
            ErrorCode::Busy
        );
        release_tx.send(()).expect("worker waiting");
        // Confirm completion by awaiting a second admitted job; the running closure owns the permit.
        let next = tokio::time::timeout(LIMIT, async {
            loop {
                match executor.submit(|| 7) {
                    Ok(job) => break job,
                    Err(error) if error.code == ErrorCode::Busy => tokio::task::yield_now().await,
                    Err(error) => panic!("unexpected submit error: {error}"),
                }
            }
        })
        .await
        .expect("worker completes and releases capacity");
        assert_eq!(result(next).await.expect("capacity released"), 7);
    }

    #[tokio::test]
    async fn a_stuck_job_does_not_block_a_different_executor() {
        let first_executor = executor(1);
        let second_executor = executor(1);
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = mpsc::sync_channel::<()>(1);
        let first = first_executor
            .submit(move || {
                let _ = started_tx.send(());
                release_rx.recv_timeout(LIMIT).is_ok()
            })
            .expect("first admitted");
        tokio::time::timeout(LIMIT, started_rx)
            .await
            .expect("first started")
            .expect("signal");
        let second = second_executor
            .submit(|| 2)
            .expect("independent executor admits");
        assert_eq!(result(second).await.expect("second succeeds"), 2);
        release_tx.send(()).expect("first waits");
        assert!(result(first).await.expect("first succeeds"));
    }

    #[test]
    fn queued_work_is_skipped_when_its_job_is_dropped() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .max_blocking_threads(1)
            .enable_all()
            .build()
            .expect("test runtime");
        runtime.block_on(async {
            let (block_started_tx, block_started_rx) = tokio::sync::oneshot::channel();
            let (release_tx, release_rx) = mpsc::sync_channel::<()>(1);
            let blocker = tokio::task::spawn_blocking(move || {
                let _ = block_started_tx.send(());
                let _ = release_rx.recv_timeout(LIMIT);
            });
            tokio::time::timeout(LIMIT, block_started_rx)
                .await
                .expect("blocker started")
                .expect("signal");

            // Jobs admitted by this executor are queued behind the blocker; drop cancels that queue entry.
            let executor = executor(1);
            let ran = Arc::new(AtomicBool::new(false));
            let ran_by_work = Arc::clone(&ran);
            let dropped = executor
                .submit(move || ran_by_work.store(true, Ordering::SeqCst))
                .expect("queued");
            drop(dropped);
            release_tx.send(()).expect("blocker waiting");
            tokio::time::timeout(LIMIT, blocker)
                .await
                .expect("blocker exits")
                .expect("blocking task joins");
            let (sentinel_tx, sentinel_rx) = tokio::sync::oneshot::channel();
            let sentinel = tokio::task::spawn_blocking(move || {
                let _ = sentinel_tx.send(());
            });
            tokio::time::timeout(LIMIT, sentinel_rx)
                .await
                .expect("queue drained")
                .expect("sentinel ran");
            tokio::time::timeout(LIMIT, sentinel)
                .await
                .expect("sentinel exits")
                .expect("sentinel joins");
            assert!(
                !ran.load(Ordering::SeqCst),
                "canceled queued work was skipped"
            );
            assert_eq!(
                result(
                    executor
                        .submit(|| 1)
                        .expect("queued cancellation released budget")
                )
                .await
                .expect("slot is reusable"),
                1
            );
        });
    }

    #[tokio::test]
    async fn a_capacity_of_two_admits_two_outstanding_jobs_and_rejects_the_third() {
        let executor = executor(2);
        let (release_tx, release_rx) = mpsc::sync_channel::<()>(2);
        let release_rx = Arc::new(std::sync::Mutex::new(release_rx));
        let mut running = Vec::new();
        for _ in 0..2 {
            let release_rx = Arc::clone(&release_rx);
            running.push(
                executor
                    .submit(move || {
                        let _ = release_rx.lock().unwrap().recv_timeout(LIMIT);
                    })
                    .expect("admitted within the capacity"),
            );
        }
        match executor.submit(|| ()) {
            Err(error) => assert_eq!(error.code, ErrorCode::Busy),
            Ok(_) => panic!("a third outstanding job was admitted at capacity two"),
        }
        release_tx.send(()).expect("first worker waits");
        release_tx.send(()).expect("second worker waits");
        for job in running {
            result(job).await.expect("worker finishes");
        }
    }

    #[tokio::test]
    async fn a_full_executor_rejects_and_drops_the_new_work() {
        struct Witness(Arc<AtomicBool>);
        impl Drop for Witness {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        let executor = executor(1);
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = mpsc::sync_channel::<()>(1);
        let running = executor
            .submit(move || {
                let _ = started_tx.send(());
                let _ = release_rx.recv_timeout(LIMIT);
            })
            .expect("admitted");
        tokio::time::timeout(LIMIT, started_rx)
            .await
            .expect("worker started")
            .expect("signal");
        let dropped = Arc::new(AtomicBool::new(false));
        let witness = Witness(Arc::clone(&dropped));
        match executor.submit(move || drop(witness)) {
            Err(error) => assert_eq!(error.code, ErrorCode::Busy),
            Ok(job) => {
                drop(job);
                panic!("full executor admitted work");
            }
        }
        assert!(dropped.load(Ordering::SeqCst));
        release_tx.send(()).expect("worker waits");
        result(running).await.expect("worker finishes");
    }

    #[tokio::test]
    async fn cloned_executors_share_the_same_admission_budget() {
        let executor = executor(1);
        let clone = executor.clone();
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = mpsc::sync_channel::<()>(1);
        let running = executor
            .submit(move || {
                let _ = started_tx.send(());
                let _ = release_rx.recv_timeout(LIMIT);
            })
            .expect("admitted");
        tokio::time::timeout(LIMIT, started_rx)
            .await
            .expect("worker started")
            .expect("signal");
        assert_eq!(
            clone.submit(|| ()).err().expect("shared limit").code,
            ErrorCode::Busy
        );
        release_tx.send(()).expect("worker waiting");
        result(running).await.expect("running work completes");
    }

    #[test]
    fn queued_jobs_count_towards_the_admission_limit() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .max_blocking_threads(1)
            .enable_all()
            .build()
            .expect("test runtime");
        runtime.block_on(async {
            let (started_tx, started_rx) = tokio::sync::oneshot::channel();
            let (release_tx, release_rx) = mpsc::sync_channel::<()>(1);
            let blocker = tokio::task::spawn_blocking(move || {
                let _ = started_tx.send(());
                let _ = release_rx.recv_timeout(LIMIT);
            });
            tokio::time::timeout(LIMIT, started_rx)
                .await
                .expect("blocker started")
                .expect("signal");
            let executor = executor(1);
            let queued = executor.submit(|| 5).expect("queued job admitted");
            assert_eq!(
                executor
                    .submit(|| 6)
                    .err()
                    .expect("queued job owns permit")
                    .code,
                ErrorCode::Busy
            );
            release_tx.send(()).expect("blocker waiting");
            tokio::time::timeout(LIMIT, blocker)
                .await
                .expect("blocker exits")
                .expect("join");
            assert_eq!(result(queued).await.expect("queued job completes"), 5);
        });
    }

    #[test]
    fn dropping_a_queued_job_drops_its_captured_work() {
        struct Witness(Arc<AtomicBool>);
        impl Drop for Witness {
            fn drop(&mut self) {
                self.0.store(true, Ordering::SeqCst);
            }
        }
        let runtime = tokio::runtime::Builder::new_current_thread()
            .max_blocking_threads(1)
            .enable_all()
            .build()
            .expect("test runtime");
        runtime.block_on(async {
            let (started_tx, started_rx) = tokio::sync::oneshot::channel();
            let (release_tx, release_rx) = mpsc::sync_channel::<()>(1);
            let blocker = tokio::task::spawn_blocking(move || {
                let _ = started_tx.send(());
                let _ = release_rx.recv_timeout(LIMIT);
            });
            tokio::time::timeout(LIMIT, started_rx)
                .await
                .expect("blocker started")
                .expect("signal");
            let executor = executor(1);
            let dropped = Arc::new(AtomicBool::new(false));
            let witness = Witness(Arc::clone(&dropped));
            let job = executor.submit(move || drop(witness)).expect("queued");
            drop(job);
            assert!(
                !dropped.load(Ordering::SeqCst),
                "queued closure still owns payload"
            );
            release_tx.send(()).expect("blocker waiting");
            tokio::time::timeout(LIMIT, blocker)
                .await
                .expect("blocker exits")
                .expect("join");
            let (sentinel_tx, sentinel_rx) = tokio::sync::oneshot::channel();
            let sentinel = tokio::task::spawn_blocking(move || {
                let _ = sentinel_tx.send(());
            });
            tokio::time::timeout(LIMIT, sentinel_rx)
                .await
                .expect("queue drained")
                .expect("sentinel ran");
            tokio::time::timeout(LIMIT, sentinel)
                .await
                .expect("sentinel exits")
                .expect("join");
            assert!(
                dropped.load(Ordering::SeqCst),
                "queued closure and payload were dropped"
            );
        });
    }

    #[tokio::test]
    async fn completed_jobs_release_capacity_before_delivering_the_result() {
        let executor = executor(1);
        for expected in 0..20 {
            let value = result(
                executor
                    .submit(move || expected)
                    .expect("capacity available"),
            )
            .await
            .expect("job completes");
            assert_eq!(value, expected);
        }
    }
}
