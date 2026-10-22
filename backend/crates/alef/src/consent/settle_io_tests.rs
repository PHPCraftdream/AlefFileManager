// SPDX-License-Identifier: MIT OR Apache-2.0
//! The store I/O of the settlement on an executor: a poll before its turn stays pending and no
//! file appears; a queued write cancelled with its caller never runs. Each test owns its executor
//! and the only blocking thread of its runtime, so no wait on another test can change the outcome.
use std::time::Duration;

use super::*;

/// Holds the only blocking thread of the runtime until released: a submitted job is admitted but
/// cannot start. Release through the sender; join the returned handle at the end.
async fn hold_the_blocking_thread() -> (std::sync::mpsc::Sender<()>, tokio::task::JoinHandle<()>) {
    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
    let blocker = tokio::task::spawn_blocking(move || {
        let _ = started_tx.send(());
        let _ = release_rx.recv_timeout(Duration::from_secs(30));
    });
    tokio::time::timeout(Duration::from_secs(5), started_rx)
        .await
        .expect("the blocker starts")
        .expect("the blocker signals");
    (release_tx, blocker)
}

/// The first poll on a bare waker: `true` while the work waits for the executor. An inline
/// implementation would finish in this poll.
fn polls_pending<F: std::future::Future>(future: &mut std::pin::Pin<Box<F>>) -> bool {
    let waker = std::task::Waker::noop();
    let mut cx = std::task::Context::from_waker(waker);
    future.as_mut().poll(&mut cx).is_pending()
}

fn test_executor() -> BlockingExecutor {
    BlockingExecutor::new(std::num::NonZeroUsize::new(2).expect("non-zero test limit"))
}

#[test]
fn the_store_write_waits_for_its_turn_on_the_executor_and_does_not_run_inline() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .max_blocking_threads(1)
        .enable_all()
        .build()
        .expect("the test runtime starts");
    runtime.block_on(async {
        let executor = test_executor();
        let (release_tx, blocker) = hold_the_blocking_thread().await;
        let scratch = tempfile::tempdir().unwrap();
        let store = FileConsentStore::new(scratch.path().join("consent"));
        let identity = Identity {
            app_id: "org.example.placement".to_owned(),
            signer: Some("dir:example".to_owned()),
        };
        let mut consent = Consent::undecided();
        consent.set(Right::plain("clipboard.read"), Decision::Substitute);
        let written = scratch
            .path()
            .join("consent")
            .join(format!("{}.ktav", identity.key()));
        let mut write = Box::pin(store_decisions(&executor, &store, &identity, &consent));
        assert!(
            polls_pending(&mut write),
            "an inline write would finish in the first poll"
        );
        assert!(!written.exists(), "the store was written before its turn");
        release_tx.send(()).expect("the blocker is released");
        tokio::time::timeout(Duration::from_secs(5), write.as_mut())
            .await
            .expect("the write finishes after the release")
            .expect("the write succeeds");
        assert!(written.exists(), "the write lands after its turn");
        tokio::time::timeout(Duration::from_secs(5), blocker)
            .await
            .expect("the blocker exits")
            .expect("the blocker joins");
    });
}

#[test]
fn the_store_read_waits_for_its_turn_on_the_executor_and_does_not_run_inline() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .max_blocking_threads(1)
        .enable_all()
        .build()
        .expect("the test runtime starts");
    runtime.block_on(async {
        let executor = test_executor();
        let (release_tx, blocker) = hold_the_blocking_thread().await;
        let scratch = tempfile::tempdir().unwrap();
        let store = FileConsentStore::new(scratch.path().join("consent"));
        let identity = Identity {
            app_id: "org.example.placement".to_owned(),
            signer: Some("dir:example".to_owned()),
        };
        let mut stored = Consent::undecided();
        stored.set(Right::plain("clipboard.read"), Decision::Deny);
        // The setup is the store's own synchronous work, not the path under test.
        store.save(&identity, &stored).unwrap();
        let mut read = Box::pin(stored_decisions(&executor, &store, &identity));
        assert!(
            polls_pending(&mut read),
            "an inline read would finish in the first poll"
        );
        release_tx.send(()).expect("the blocker is released");
        let read_back = tokio::time::timeout(Duration::from_secs(5), read.as_mut())
            .await
            .expect("the read finishes after the release")
            .expect("the read succeeds");
        assert_eq!(read_back, Some(stored));
        tokio::time::timeout(Duration::from_secs(5), blocker)
            .await
            .expect("the blocker exits")
            .expect("the blocker joins");
    });
}

#[test]
fn a_queued_store_write_cancelled_with_its_caller_never_runs() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .max_blocking_threads(1)
        .enable_all()
        .build()
        .expect("the test runtime starts");
    runtime.block_on(async {
        // Two slots: the queued job of the caller holds one until its turn, the sentinel takes
        // the other without meeting a full budget.
        let executor = test_executor();
        let (release_tx, blocker) = hold_the_blocking_thread().await;
        let scratch = tempfile::tempdir().unwrap();
        let store = FileConsentStore::new(scratch.path().join("consent"));
        let identity = Identity {
            app_id: "org.example.cancel".to_owned(),
            signer: Some("dir:example".to_owned()),
        };
        let mut consent = Consent::undecided();
        consent.set(Right::plain("clipboard.read"), Decision::Deny);
        let written = scratch
            .path()
            .join("consent")
            .join(format!("{}.ktav", identity.key()));
        let mut write = Box::pin(store_decisions(&executor, &store, &identity, &consent));
        assert!(
            polls_pending(&mut write),
            "an inline write would finish in the first poll"
        );
        drop(write); // the caller is gone: the queued job must be skipped
        release_tx.send(()).expect("the blocker is released");
        // The sentinel proves the queue drained past the cancelled job.
        let sentinel = executor.submit(|| 7).expect("the sentinel is admitted");
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(5), sentinel.result())
                .await
                .expect("the queue drains")
                .expect("the sentinel runs"),
            7
        );
        assert!(!written.exists(), "the cancelled write must not have run");
        tokio::time::timeout(Duration::from_secs(5), blocker)
            .await
            .expect("the blocker exits")
            .expect("the blocker joins");
    });
}
