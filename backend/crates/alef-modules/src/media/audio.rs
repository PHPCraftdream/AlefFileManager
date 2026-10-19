// SPDX-License-Identifier: MIT OR Apache-2.0
//! The `audio` commands: opening a sink the page writes its samples to. Opening needs the `audio`
//! permission; the samples arrive as the body of `audio.write` and are only counted, never kept.
use std::{future::Future, pin::Pin, sync::Arc, sync::Mutex};

use alef_core::session::BlockingExecutor;
use alef_core::{
    ids::{ResourceId, StreamId},
    registry::{context::CallContext, dispatch::Registry},
    security::permissions::Permission,
    session::resources::Resource,
    AlefError, ErrorCode,
};
use bytes::Bytes;
use serde::Deserialize;
use serde_json::json;

use crate::{json, media::invalid};

use super::{device::backend::AudioBackend, substituted};

/// The fold's outcome, published exactly once by the publisher task: `Ok(())` after the input
/// drained, or the error the fold ended with (a refused chunk, a panic, a canceled job). Closers
/// only wait for the cell; a timeout never writes into it, so a later closer waits on the same
/// completion instead of reporting partial statistics.
#[derive(Default)]
struct DoneState {
    outcome: Mutex<Option<Result<(), AlefError>>>,
    notify: tokio::sync::Notify,
}

impl DoneState {
    /// Publishes the first outcome; later publications (there should be none) are ignored.
    fn publish(&self, outcome: Result<(), AlefError>) {
        let mut cell = self.outcome.lock().unwrap_or_else(|e| e.into_inner());
        if cell.is_none() {
            *cell = Some(outcome);
        }
        drop(cell);
        self.notify.notify_waiters();
    }

    /// Resolves with the published outcome; never resolves while the fold is still running.
    async fn settled(&self) -> Result<(), AlefError> {
        loop {
            let notified = self.notify.notified();
            tokio::pin!(notified);
            notified.as_mut().enable();
            if let Some(outcome) = self
                .outcome
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .clone()
            {
                return outcome;
            }
            notified.as_mut().await;
        }
    }
}

/// How long a close waits for the fold to drain before it answers `TIMEOUT`.
const DRAIN_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// How long the resource cleanup waits for each step (the fold, the forwarder, the publisher).
const CLEANUP_WAIT: std::time::Duration = std::time::Duration::from_secs(5);

/// Awaits both tasks of an opened sink, then removes and drops the resource (the drain above
/// already did its teardown work; its close would wait on the same `closing` lock).
async fn tear_down(ctx: &CallContext, id: ResourceId) {
    let (forwarder, publisher) = ctx
        .resources()
        .with_as::<AudioSinkHandle, _>(id, |handle| {
            (
                handle
                    .forwarder
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .take(),
                handle
                    .publisher
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .take(),
            )
        })
        .unwrap_or((None, None));
    let _ = tokio::time::timeout(DRAIN_TIMEOUT, async move {
        if let Some(forwarder) = forwarder {
            let _ = forwarder.await;
        }
        if let Some(publisher) = publisher {
            let _ = publisher.await;
        }
    })
    .await;
    if let Ok(resource) = ctx.resources().take(id) {
        drop(resource);
    }
}

/// A sink the page opened. The checksum runs in an admitted blocking job (the fold) that owns
/// the sink writes for the stream's lifetime; closing it (explicitly, or at the teardown of the
/// session) terminates the input, drains everything the consumer accepted, waits for the fold's
/// outcome and joins both tasks, so a later write answers `Closed`. Closes serialize on
/// `closing`: a concurrent close waits on the same completion instead of bypassing it.
struct AudioSinkHandle {
    sink: super::device::backend::AudioSink,
    stream_id: StreamId,
    /// The fold's shared completion; closers wait on it and never take it away.
    done: Arc<DoneState>,
    closing: Arc<tokio::sync::Mutex<()>>,
    /// The chunk forwarder; kept so the teardown can await it.
    forwarder: Mutex<Option<tokio::task::JoinHandle<()>>>,
    /// The fold's publisher: holds the fold's result receiver from spawn, so the executor never
    /// sees the receiver dropped while the fold is still queued, and turns the fold's outcome
    /// (write error, panic, cancellation) into the shared cell.
    publisher: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl Resource for AudioSinkHandle {
    fn close(self: Box<Self>) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        Box::pin(async move {
            let _guard = self.closing.lock().await;
            // Session::close terminates the streams before the resources; the forwarder then ends,
            // the fold drains the handoff and the publisher records the outcome. A fold that is
            // still unsettled after the wait is cut off here: aborting the forwarder drops the
            // reader (the input ends) and the handoff sender (the fold ends after what it holds),
            // and an abandoned publisher is aborted too, so no task outlives the resource.
            let settled = tokio::time::timeout(CLEANUP_WAIT, self.done.settled())
                .await
                .is_ok();
            let forwarder = self
                .forwarder
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take();
            if let Some(forwarder) = forwarder {
                if !settled {
                    forwarder.abort();
                }
                let _ = tokio::time::timeout(CLEANUP_WAIT, forwarder).await;
            }
            let publisher = self
                .publisher
                .lock()
                .unwrap_or_else(|e| e.into_inner())
                .take();
            if let Some(mut publisher) = publisher {
                if tokio::time::timeout(CLEANUP_WAIT, &mut publisher)
                    .await
                    .is_err()
                {
                    publisher.abort();
                }
            }
            let _ = self.sink.try_close();
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteArgs {
    sink: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CloseArgs {
    sink: u64,
}

pub(crate) fn register(
    registry: &mut Registry,
    backend: Arc<dyn AudioBackend>,
) -> Result<(), AlefError> {
    register_on(registry, backend, crate::media::executor())
}

/// The same commands on an explicit budget; the tests use an isolated one.
pub(crate) fn register_on(
    registry: &mut Registry,
    backend: Arc<dyn AudioBackend>,
    executor: &BlockingExecutor,
) -> Result<(), AlefError> {
    let executor = executor.clone();
    let open = backend.clone();
    registry
        .command::<()>("audio.open")?
        .permission(Permission::Audio, |_| None)
        .substitutes()
        .handler(move |ctx, ()| {
            let backend = open.clone();
            let executor = executor.clone();
            async move {
                substituted(&ctx, "audio")?;
                // The sink opens on the budget, and the fold job is admitted before the consumer
                // exists, so a full budget refuses with `Busy` and nothing is created.
                let sink = executor.submit(move || backend.open())?.result().await??;
                let (mut reader, stream_id) = ctx.streams().open_incoming_reader();
                // A small handoff: the fold may fall a few chunks behind, and then the bounded
                // incoming stream slows the page instead of buffering without limit.
                let (chunks_tx, mut chunks_rx) = tokio::sync::mpsc::channel::<Bytes>(8);
                let done = Arc::new(DoneState::default());
                let folding = sink.clone();
                let fold = match executor.submit(move || {
                    // The fold owns the checksum for the stream's lifetime: every accepted chunk
                    // is written here, off the Tokio worker, until the input ends or the sink
                    // refuses.
                    loop {
                        match chunks_rx.blocking_recv() {
                            Some(chunk) => {
                                if let Err(error) = folding.write(&chunk) {
                                    break Err(error);
                                }
                            }
                            None => break Ok(()),
                        }
                    }
                }) {
                    Ok(fold) => fold,
                    Err(error) => {
                        let _ = ctx.streams().close(stream_id);
                        return Err(error);
                    }
                };
                // The publisher holds the fold's result receiver from spawn (so a queued fold is
                // never skipped) and turns the fold's outcome into the shared cell.
                let publisher = tokio::spawn({
                    let done = done.clone();
                    async move {
                        let outcome = fold.result().await.and_then(|outcome| outcome);
                        done.publish(outcome);
                    }
                });
                let forwarder = tokio::spawn(async move {
                    while let Some(Ok(chunk)) = reader.recv().await {
                        if chunks_tx.send(chunk).await.is_err() {
                            break;
                        }
                    }
                });
                let sink_id = match ctx.resources().insert(Box::new(AudioSinkHandle {
                    sink: sink.clone(),
                    stream_id,
                    done,
                    closing: Arc::new(tokio::sync::Mutex::new(())),
                    forwarder: Mutex::new(Some(forwarder)),
                    publisher: Mutex::new(Some(publisher)),
                })) {
                    Ok(sink_id) => sink_id,
                    // The box drops and the tasks cascade to an end: the stream closes, the
                    // forwarder ends, the sender drops, the fold ends, the publisher publishes.
                    Err(error) => {
                        let _ = ctx.streams().close(stream_id);
                        return Err(error);
                    }
                };
                json(&json!({ "sink": sink_id.0, "stream": stream_id.0 }))
            }
        })?;
    registry
        .command::<WriteArgs>("audio.write")?
        .handler(move |ctx, args| async move {
            let body = ctx
                .body()
                .ok_or_else(|| invalid("audio.write needs its samples as the body".to_owned()))?
                .clone();
            let sink = ctx
                .resources()
                .with_as::<AudioSinkHandle, _>(ResourceId(args.sink), |handle| handle.sink.clone())
                .map_err(|_| {
                    AlefError::new(ErrorCode::Closed, "the audio sink is closed already")
                })?;
            let written = crate::media::executor()
                .submit(move || sink.write(&body))?
                .result()
                .await??;
            json(&json!({ "sink": args.sink, "samples": written }))
        })?;
    registry
        .command::<CloseArgs>("audio.close")?
        .handler(move |ctx, args| async move {
            let id = ResourceId(args.sink);
            // Close drains the queued writes first and refuses unknown or foreign ids with
            // NOT_FOUND, leaving the resource alone.
            let (sink, stream_id, done, closing) =
                ctx.resources()
                    .with_as::<AudioSinkHandle, _>(id, |handle| {
                        (
                            handle.sink.clone(),
                            handle.stream_id,
                            handle.done.clone(),
                            handle.closing.clone(),
                        )
                    })?;
            // One closer at a time: a concurrent close waits here and reports the same outcome.
            let _guard = closing.lock().await;
            // Terminate the input; the reader drains what is queued, the forwarder finishes, the
            // fold drains the handoff and the publisher records the outcome.
            ctx.streams().close(stream_id)?;
            // A timeout leaves the cell and both task handles in the resource: a later close (or
            // the teardown) waits on the same completion instead of reporting partial statistics.
            match tokio::time::timeout(DRAIN_TIMEOUT, done.settled()).await {
                Err(_elapsed) => {
                    return Err(AlefError::new(
                        ErrorCode::Timeout,
                        "the audio sink did not drain in time",
                    ));
                }
                // A receiver error is never swallowed: a canceled or panicked fold is a real
                // error. Closers already waiting on the guard read it from the same cell; once
                // the resource is gone a later closer gets `NOT_FOUND`, like any closed id.
                Ok(Err(error)) => {
                    tear_down(&ctx, id).await;
                    return Err(error);
                }
                Ok(Ok(())) => {}
            }
            // The drain above did the teardown work; drop the resource without closing it again
            // (its close would wait on the same `closing` lock).
            tear_down(&ctx, id).await;
            let (samples, checksum) = sink.try_close()?;
            // The checksum goes out as hex: a u64 does not survive a JSON number exactly.
            json(&json!({
                "sink": args.sink,
                "samples": samples,
                "checksum": super::device::backend::AudioSink::checksum_hex(checksum),
            }))
        })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::virtuals::VirtualAudio;
    use alef_core::{
        protocol::call::Limits,
        registry::{command::Reply, context::CallContext},
        security::{
            consent::Consent,
            manifest,
            permissions::{PathVars, PermissionSet},
        },
        session::{session::Session, session::SessionManager},
    };
    use bytes::Bytes;

    const WAIT: std::time::Duration = std::time::Duration::from_secs(10);

    async fn session() -> Arc<Session> {
        SessionManager::new(Arc::new(|| "test-token".to_owned()), Limits::default())
            .begin_document(1)
            .await
    }

    fn path_vars() -> PathVars {
        let path = std::env::temp_dir();
        PathVars {
            app_data: path.clone(),
            app_config: path.clone(),
            app_cache: path.clone(),
            home: path.clone(),
            documents: path.clone(),
            downloads: path.clone(),
            desktop: path.clone(),
            temp: path.clone(),
            app: Some(path),
        }
    }

    fn permissions(audio: bool) -> Arc<PermissionSet> {
        let policy: manifest::Permissions = serde_json::from_value(json!({
            "fs": {"read": [], "write": []},
            "cli": {},
            "net": {"http": [], "socket": []},
            "shell": {"openExternal": []},
            "clipboard": {"read": false},
            "shortcut": {"global": false},
            "secrets": false,
            "app": {"env": []},
            "audio": audio,
        }))
        .unwrap();
        Arc::new(
            PermissionSet::from_manifest(&policy, &path_vars())
                .unwrap()
                .with_consent(Consent::allow_all()),
        )
    }

    async fn opened() -> (Registry, CallContext, u64) {
        let mut registry = Registry::default();
        register(&mut registry, Arc::new(VirtualAudio)).unwrap();
        let ctx = CallContext::new(session().await, permissions(true));
        let reply = registry
            .dispatch("audio.open", ctx.clone(), serde_json::Value::Null)
            .await
            .expect("the open must succeed");
        let Reply::Json(reply) = reply else {
            panic!("expected a JSON reply");
        };
        (registry, ctx, reply["sink"].as_u64().unwrap())
    }

    /// An audio backend that hands out one real sink and remembers it, so a test can install the
    /// write gate on the very sink the fold owns.
    #[derive(Debug, Default)]
    struct GatedAudio {
        created: Mutex<Option<crate::media::device::backend::AudioSink>>,
    }

    impl AudioBackend for GatedAudio {
        fn open(&self) -> Result<crate::media::device::backend::AudioSink, AlefError> {
            let sink = crate::media::device::backend::AudioSink::new();
            *self.created.lock().unwrap_or_else(|e| e.into_inner()) = Some(sink.clone());
            Ok(sink)
        }
    }

    /// A latch that holds the fold's writes until the test sends a release (or drops the sender,
    /// which a failing test does); once open it stays open for every later write. The flag turns
    /// true when the fold first reaches the gate, so a test can tell that work is really pending.
    fn write_gate() -> (
        std::sync::mpsc::Sender<()>,
        std::sync::Arc<dyn Fn() + Send + Sync>,
        Arc<std::sync::atomic::AtomicBool>,
    ) {
        use std::sync::atomic::{AtomicBool, Ordering};
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let release_rx = Arc::new(Mutex::new(release_rx));
        let opened = Arc::new(AtomicBool::new(false));
        let entered = Arc::new(AtomicBool::new(false));
        let gate = {
            let entered = entered.clone();
            Arc::new(move || {
                entered.store(true, Ordering::SeqCst);
                if opened.load(Ordering::SeqCst) {
                    return;
                }
                let _ = release_rx.lock().unwrap_or_else(|e| e.into_inner()).recv();
                opened.store(true, Ordering::SeqCst);
            })
        };
        (release_tx, gate, entered)
    }

    /// Waits until the fold is parked in the gate, so the queued work is really pending.
    async fn parked(entered: &std::sync::atomic::AtomicBool) {
        tokio::time::timeout(WAIT, async {
            while !entered.load(std::sync::atomic::Ordering::SeqCst) {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the fold never reached the gate");
    }

    /// Opens a sink on a gated backend behind a budget-of-one executor, so the fold's admission
    /// is observable; answers the registry, the context, the backend and the sink and stream ids.
    async fn opened_gated() -> (Registry, CallContext, Arc<GatedAudio>, u64, u64) {
        let mut registry = Registry::default();
        let gated = Arc::new(GatedAudio::default());
        let executor =
            BlockingExecutor::new(std::num::NonZeroUsize::new(1).expect("non-zero test limit"));
        register_on(&mut registry, gated.clone(), &executor).unwrap();
        let ctx = CallContext::new(session().await, permissions(true));
        let reply = registry
            .dispatch("audio.open", ctx.clone(), serde_json::Value::Null)
            .await
            .expect("the open must succeed");
        let Reply::Json(reply) = reply else {
            panic!("expected a JSON reply");
        };
        (
            registry,
            ctx,
            gated,
            reply["sink"].as_u64().unwrap(),
            reply["stream"].as_u64().unwrap(),
        )
    }

    fn fnv(bytes: &[u8]) -> u64 {
        let mut checksum = 0xcbf2_9ce4_8422_2325;
        for byte in bytes {
            checksum ^= u64::from(*byte);
            checksum = checksum.wrapping_mul(0x0000_0100_0000_01b3);
        }
        checksum
    }

    #[tokio::test]
    async fn written_samples_are_counted_and_the_checksum_is_reported_as_hex() {
        let (registry, ctx, sink) = opened().await;
        let first = Bytes::from(vec![0u8, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]);
        let reply = registry
            .dispatch(
                "audio.write",
                ctx.clone().with_body(Some(first.clone())),
                json!({"sink": sink}),
            )
            .await
            .expect("the first write must succeed");
        let Reply::Json(reply) = reply else {
            panic!("expected a JSON reply");
        };
        assert_eq!(reply["samples"], 4);
        let invalid = registry
            .dispatch(
                "audio.write",
                ctx.clone().with_body(Some(Bytes::from_static(&[1, 2, 3]))),
                json!({"sink": sink}),
            )
            .await
            .expect_err("a 3-byte body must be refused");
        assert_eq!(invalid.code, ErrorCode::InvalidArgument);
        let second = Bytes::from(vec![16u8, 17, 18, 19, 20, 21, 22, 23]);
        let reply = registry
            .dispatch(
                "audio.write",
                ctx.clone().with_body(Some(second.clone())),
                json!({"sink": sink}),
            )
            .await
            .expect("the second write must succeed");
        let Reply::Json(reply) = reply else {
            panic!("expected a JSON reply");
        };
        assert_eq!(reply["samples"], 6);
        let mut heard = first.to_vec();
        heard.extend_from_slice(&second);
        let reply = registry
            .dispatch("audio.close", ctx.clone(), json!({"sink": sink}))
            .await
            .expect("the close must succeed");
        let Reply::Json(reply) = reply else {
            panic!("expected a JSON close reply");
        };
        assert_eq!(
            reply["checksum"].as_str().unwrap(),
            crate::media::device::backend::AudioSink::checksum_hex(fnv(&heard))
        );
        let error = registry
            .dispatch(
                "audio.write",
                ctx.clone().with_body(Some(first)),
                json!({"sink": sink}),
            )
            .await
            .expect_err("a closed sink must refuse");
        assert_eq!(error.code, ErrorCode::Closed);
        tokio::time::timeout(WAIT, ctx.session.close())
            .await
            .expect("the session must close in time");
    }

    #[tokio::test]
    async fn audio_close_drains_the_writes_queued_before_it() {
        let (registry, ctx, gated, sink, stream) = opened_gated().await;
        let retained = gated
            .created
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .expect("the open must have created a sink")
            .clone();
        let (release_tx, gate, entered) = write_gate();
        retained.set_write_gate(gate);
        let writer = ctx
            .streams()
            .incoming_writer(StreamId(stream))
            .expect("the stream must be there");
        let mut queued = Vec::new();
        for first in 0u8..8 {
            let chunk = vec![
                first,
                first.wrapping_add(1),
                first.wrapping_add(2),
                first.wrapping_add(3),
            ];
            queued.extend_from_slice(&chunk);
            tokio::time::timeout(WAIT, writer.write(Bytes::from(chunk)))
                .await
                .expect("the write must go through in time")
                .expect("the write must succeed");
        }
        // The fold is parked in the gate with chunk 1 and the rest queued: the close must wait
        // for the drain instead of reporting the partial statistics.
        let closing = {
            let registry = registry.clone();
            let ctx = ctx.clone();
            tokio::spawn(async move {
                tokio::time::timeout(
                    WAIT,
                    registry.dispatch("audio.close", ctx, json!({"sink": sink})),
                )
                .await
            })
        };
        parked(&entered).await;
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(
            !closing.is_finished(),
            "the close answered while writes were still queued"
        );
        release_tx.send(()).expect("the gate must still be waiting");
        let outcome = tokio::time::timeout(WAIT, closing)
            .await
            .expect("the close must finish in time")
            .expect("the close task must not panic")
            .expect("the close task must answer in time")
            .expect("the close must succeed");
        let Reply::Json(reply) = outcome else {
            panic!("expected a JSON close reply");
        };
        assert_eq!(reply["samples"], 8);
        assert_eq!(
            reply["checksum"].as_str().unwrap(),
            crate::media::device::backend::AudioSink::checksum_hex(fnv(&queued))
        );
        tokio::time::timeout(WAIT, ctx.session.close())
            .await
            .expect("the session must close in time");
    }

    #[tokio::test]
    async fn writing_after_an_explicit_close_is_refused() {
        let (registry, ctx, sink) = opened().await;
        let reply = registry
            .dispatch("audio.close", ctx.clone(), json!({"sink": sink}))
            .await
            .expect("the close must succeed");
        let Reply::Json(reply) = reply else {
            panic!("expected a JSON close reply");
        };
        assert_eq!(reply["samples"], 0);
        let error = registry
            .dispatch(
                "audio.write",
                ctx.clone()
                    .with_body(Some(Bytes::from_static(&[0, 0, 0, 0]))),
                json!({"sink": sink}),
            )
            .await
            .expect_err("a closed sink must refuse");
        assert_eq!(error.code, ErrorCode::Closed);
        tokio::time::timeout(WAIT, ctx.session.close())
            .await
            .expect("the session must close in time");
    }

    #[tokio::test]
    async fn writing_to_an_unknown_sink_is_closed() {
        let (registry, ctx, _sink) = opened().await;
        let error = registry
            .dispatch(
                "audio.write",
                ctx.clone()
                    .with_body(Some(Bytes::from_static(&[0, 0, 0, 0]))),
                json!({"sink": 999}),
            )
            .await
            .expect_err("an unknown sink must refuse");
        assert_eq!(error.code, ErrorCode::Closed);
        tokio::time::timeout(WAIT, ctx.session.close())
            .await
            .expect("the session must close in time");
    }

    async fn reply_of(
        registry: &Registry,
        ctx: &CallContext,
        command: &str,
        args: serde_json::Value,
    ) -> serde_json::Value {
        let reply = registry
            .dispatch(command, ctx.clone(), args)
            .await
            .expect("the command must succeed");
        let Reply::Json(reply) = reply else {
            panic!("expected a JSON reply");
        };
        reply
    }

    #[tokio::test]
    async fn concurrent_closes_both_observe_the_final_statistics() {
        let (registry, ctx, _sink) = opened().await;
        let reply = reply_of(&registry, &ctx, "audio.open", serde_json::Value::Null).await;
        let id = reply["sink"].as_u64().unwrap();
        let stream = reply["stream"].as_u64().unwrap();
        let writer = ctx
            .streams()
            .incoming_writer(StreamId(stream))
            .expect("the stream must be there");
        let mut queued = Vec::new();
        for first in 0u8..8 {
            let chunk = vec![
                first,
                first.wrapping_add(1),
                first.wrapping_add(2),
                first.wrapping_add(3),
            ];
            queued.extend_from_slice(&chunk);
            tokio::time::timeout(WAIT, writer.write(Bytes::from(chunk)))
                .await
                .expect("the write must go through in time")
                .expect("the write must succeed");
        }
        let (first, second) = tokio::join!(
            registry.dispatch("audio.close", ctx.clone(), json!({"sink": id})),
            registry.dispatch("audio.close", ctx.clone(), json!({"sink": id})),
        );
        let Reply::Json(first) = first.expect("the first close must succeed") else {
            panic!("expected a JSON close reply");
        };
        let Reply::Json(second) = second.expect("the second close must succeed") else {
            panic!("expected a JSON close reply");
        };
        let expected = crate::media::device::backend::AudioSink::checksum_hex(fnv(&queued));
        for reply in [&first, &second] {
            assert_eq!(reply["samples"], 8);
            assert_eq!(reply["checksum"].as_str().unwrap(), expected);
        }
        tokio::time::timeout(WAIT, ctx.session.close())
            .await
            .expect("the session must close in time");
    }

    #[tokio::test]
    async fn a_full_audio_budget_refuses_audio_open_with_busy() {
        let executor =
            BlockingExecutor::new(std::num::NonZeroUsize::new(1).expect("non-zero test limit"));
        let mut registry = Registry::default();
        register_on(&mut registry, Arc::new(VirtualAudio), &executor).unwrap();
        let ctx = CallContext::new(session().await, permissions(true));
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let blocker = executor
            .submit(move || {
                let _ = release_rx.recv_timeout(std::time::Duration::from_secs(10));
            })
            .expect("the blocker must be admitted");
        let error = registry
            .dispatch("audio.open", ctx.clone(), serde_json::Value::Null)
            .await
            .expect_err("a full budget must refuse the open");
        assert_eq!(error.code, ErrorCode::Busy);
        release_tx.send(()).expect("the blocker must be waiting");
        tokio::time::timeout(WAIT, blocker.result())
            .await
            .expect("the blocker must finish in time")
            .expect("the blocker must succeed");
        tokio::time::timeout(WAIT, ctx.session.close())
            .await
            .expect("the session must close in time");
    }

    #[tokio::test]
    async fn teardown_joins_a_consumer_with_queued_writes() {
        let (registry, ctx, gated, _sink, stream) = opened_gated().await;
        let retained = gated
            .created
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .expect("the open must have created a sink")
            .clone();
        let (release_tx, gate, entered) = write_gate();
        retained.set_write_gate(gate);
        let writer = ctx
            .streams()
            .incoming_writer(StreamId(stream))
            .expect("the stream must be there");
        let mut queued = Vec::new();
        for first in 0u8..8 {
            let chunk = vec![
                first,
                first.wrapping_add(1),
                first.wrapping_add(2),
                first.wrapping_add(3),
            ];
            queued.extend_from_slice(&chunk);
            tokio::time::timeout(WAIT, writer.write(Bytes::from(chunk)))
                .await
                .expect("the write must go through in time")
                .expect("the write must succeed");
        }
        // The fold is parked in the gate with chunk 1 and the rest queued, so the teardown has
        // real work to join; only after the gate is released may it finish.
        let teardown = tokio::spawn({
            let teardown_session = ctx.session.clone();
            async move { tokio::time::timeout(WAIT, teardown_session.close()).await }
        });
        parked(&entered).await;
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        assert!(
            !teardown.is_finished(),
            "the teardown finished while writes were still queued"
        );
        release_tx.send(()).expect("the gate must still be waiting");
        tokio::time::timeout(WAIT, teardown)
            .await
            .expect("the teardown must finish in time")
            .expect("the teardown task must not panic")
            .expect("the session must close in time");
        // Every queued chunk was written before the sink closed.
        assert_eq!(retained.stats(), (8, fnv(&queued)));
        // The fold's admission permit is free: a new session can open on the same executor.
        let second_ctx = CallContext::new(
            SessionManager::new(Arc::new(|| "test-token".to_owned()), Limits::default())
                .begin_document(2)
                .await,
            permissions(true),
        );
        let reply = reply_of(
            &registry,
            &second_ctx,
            "audio.open",
            serde_json::Value::Null,
        )
        .await;
        assert!(reply["sink"].as_u64().is_some());
        assert!(reply["stream"].as_u64().is_some());
        tokio::time::timeout(WAIT, second_ctx.session.close())
            .await
            .expect("the second session must close in time");
    }

    #[tokio::test]
    async fn a_timed_out_close_does_not_let_a_later_close_report_partial_statistics() {
        let (registry, ctx, gated, sink, stream) = opened_gated().await;
        let retained = gated
            .created
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .as_ref()
            .expect("the open must have created a sink")
            .clone();
        let (release_tx, gate, entered) = write_gate();
        retained.set_write_gate(gate);
        let writer = ctx
            .streams()
            .incoming_writer(StreamId(stream))
            .expect("the stream must be there");
        let mut queued = Vec::new();
        for first in 0u8..8 {
            let chunk = vec![
                first,
                first.wrapping_add(1),
                first.wrapping_add(2),
                first.wrapping_add(3),
            ];
            queued.extend_from_slice(&chunk);
            tokio::time::timeout(WAIT, writer.write(Bytes::from(chunk)))
                .await
                .expect("the write must go through in time")
                .expect("the write must succeed");
        }
        let first = {
            let registry = registry.clone();
            let ctx = ctx.clone();
            tokio::spawn(async move {
                tokio::time::timeout(
                    WAIT,
                    registry.dispatch("audio.close", ctx, json!({"sink": sink})),
                )
                .await
            })
        };
        parked(&entered).await;
        tokio::time::sleep(std::time::Duration::from_millis(100)).await;
        // The second close waits on the same completion behind the first one; it must never
        // report the partial 8-chunk statistics as a success.
        let second = {
            let registry = registry.clone();
            let ctx = ctx.clone();
            tokio::spawn(async move {
                tokio::time::timeout(
                    std::time::Duration::from_secs(12),
                    registry.dispatch("audio.close", ctx, json!({"sink": sink})),
                )
                .await
            })
        };
        let first = tokio::time::timeout(std::time::Duration::from_secs(15), first)
            .await
            .expect("the first close must finish in time")
            .expect("the first close task must not panic")
            .expect("the first close task must answer in time")
            .expect_err("the first close must time out");
        assert_eq!(first.code, ErrorCode::Timeout);
        let second = tokio::time::timeout(std::time::Duration::from_secs(15), second)
            .await
            .expect("the second close must finish in time")
            .expect("the second close task must not panic")
            .expect("the second close task must answer in time")
            .expect_err("the second close must time out too");
        assert_eq!(second.code, ErrorCode::Timeout);
        release_tx.send(()).expect("the gate must still be waiting");
        let reply = reply_of(&registry, &ctx, "audio.close", json!({"sink": sink})).await;
        assert_eq!(reply["samples"], 8);
        assert_eq!(
            reply["checksum"].as_str().unwrap(),
            crate::media::device::backend::AudioSink::checksum_hex(fnv(&queued))
        );
        tokio::time::timeout(WAIT, ctx.session.close())
            .await
            .expect("the session must close in time");
    }
}
