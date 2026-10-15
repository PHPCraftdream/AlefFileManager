// SPDX-License-Identifier: MIT OR Apache-2.0
//! Terminal commands and their session-owned process.
#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;
#[cfg(unix)]
use unix as platform;
#[cfg(windows)]
use windows as platform;

use super::{
    spawn::{Process, Shared},
    BlockingExecutor,
};
use alef_core::{
    ids::ResourceId,
    registry::{command::Reply, dispatch::Registry},
    security::{consent::Decision, permissions::Permission},
    session::session::Session,
    AlefError, ErrorCode,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
    time::Duration,
};

#[cfg(test)]
mod tests {
    use super::*;

    #[cfg(windows)]
    use super::super::executor;

    #[cfg(windows)]
    async fn session() -> std::sync::Arc<alef_core::session::session::Session> {
        use alef_core::{protocol::call::Limits, session::SessionManager};
        SessionManager::new(
            std::sync::Arc::new(|| "test-token".into()),
            Limits::default(),
        )
        .begin_document(1)
        .await
    }

    #[cfg(windows)]
    fn blocked_executor() -> (
        BlockingExecutor,
        tokio::sync::oneshot::Receiver<()>,
        std::sync::mpsc::SyncSender<()>,
        alef_core::session::BlockingJob<()>,
    ) {
        use std::num::NonZeroUsize;
        use std::sync::mpsc;
        let executor = BlockingExecutor::new(NonZeroUsize::new(1).unwrap());
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = mpsc::sync_channel::<()>(1);
        let job = executor
            .submit(move || {
                let _ = started_tx.send(());
                let _ = release_rx.recv_timeout(std::time::Duration::from_secs(5));
            })
            .expect("blocker admitted");
        (executor, started_rx, release_tx, job)
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn a_saturated_cli_executor_refuses_the_pty_open_with_busy() {
        let (executor, started_rx, release_tx, blocker) = blocked_executor();
        tokio::time::timeout(std::time::Duration::from_secs(5), started_rx)
            .await
            .expect("blocker started")
            .expect("signal");
        let error = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            open(
                &executor,
                &session().await,
                PathBuf::from("unused"),
                Vec::new(),
                std::env::temp_dir(),
                Vec::new(),
                80,
                25,
            ),
        )
        .await
        .expect("busy arrives without opening")
        .expect_err("saturated executor refuses the open");
        assert_eq!(error.code, ErrorCode::Busy);
        release_tx.send(()).expect("blocker waiting");
        drop(blocker);
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn the_pty_open_runs_on_the_blocking_executor() {
        use super::super::tree;
        let program = tree::resolve(
            "hostname",
            std::env::var_os("PATH").as_deref(),
            std::env::var_os("PATHEXT").as_deref(),
        )
        .expect("hostname resolves on the search path");
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            open(
                executor(),
                &session().await,
                program,
                Vec::new(),
                std::env::temp_dir(),
                Vec::new(),
                80,
                25,
            ),
        )
        .await
        .expect("open completes")
        .expect("pty open succeeds");
        let pid = result["pid"].as_u64().expect("pid is a number");
        assert!(pid > 0);
        // The process is registered in the session; the kill tears the tree and its pumps down.
        drop(result);
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn the_pty_open_yields_to_the_poll_before_its_open_is_admitted() {
        use std::future::Future;
        use std::num::NonZeroUsize;
        use std::task::Poll;
        let executor = BlockingExecutor::new(NonZeroUsize::new(1).unwrap());
        let session = session().await;
        let mut fut = Box::pin(open(
            &executor,
            &session,
            PathBuf::from("unused"),
            Vec::new(),
            std::env::temp_dir(),
            Vec::new(),
            80,
            25,
        ));
        let poll = fut
            .as_mut()
            .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()));
        assert!(
            matches!(poll, Poll::Pending),
            "the first poll must not open a pty"
        );
        drop(fut);
    }

    #[cfg(windows)]
    #[test]
    fn a_queued_pty_open_dropped_with_its_call_never_starts_the_program() {
        use super::super::tree;
        let runtime = tokio::runtime::Builder::new_current_thread()
            .max_blocking_threads(1)
            .enable_all()
            .build()
            .expect("test runtime");
        runtime.block_on(async {
            use std::future::Future;
            use std::num::NonZeroUsize;
            use std::sync::mpsc;
            use std::task::Poll;
            // Saturate the blocking pool itself: an admitted job of the executor queues behind it.
            let (block_started_tx, block_started_rx) = tokio::sync::oneshot::channel();
            let (release_tx, release_rx) = mpsc::sync_channel::<()>(1);
            let blocker = tokio::task::spawn_blocking(move || {
                let _ = block_started_tx.send(());
                let _ = release_rx.recv_timeout(std::time::Duration::from_secs(5));
            });
            tokio::time::timeout(std::time::Duration::from_secs(5), block_started_rx)
                .await
                .expect("blocker started")
                .expect("signal");
            let executor = BlockingExecutor::new(NonZeroUsize::new(1).unwrap());
            let program = tree::resolve(
                "powershell",
                std::env::var_os("PATH").as_deref(),
                std::env::var_os("PATHEXT").as_deref(),
            )
            .expect("powershell resolves on the search path");
            let file =
                std::env::temp_dir().join(format!("alef-pty-queued-{}.txt", std::process::id()));
            let _ = std::fs::remove_file(&file);
            let session = session().await;
            let mut fut = Box::pin(open(
                &executor,
                &session,
                program,
                vec![
                    "-NoProfile".to_owned(),
                    "-Command".to_owned(),
                    format!("Set-Content -Path '{}' -Value x", file.display()),
                ],
                std::env::temp_dir(),
                Vec::new(),
                80,
                25,
            ));
            let poll = fut
                .as_mut()
                .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()));
            assert!(matches!(poll, Poll::Pending), "the admitted open queues");
            drop(fut); // the queued open must be skipped, never started
            release_tx.send(()).expect("blocker waiting");
            // Draining the queue proves the queued closure was skipped before anything could run.
            tokio::time::timeout(std::time::Duration::from_secs(8), async {
                loop {
                    match executor.submit(|| ()) {
                        Ok(job) => break job.result().await,
                        Err(error) if error.code == ErrorCode::Busy => {
                            tokio::task::yield_now().await
                        }
                        Err(error) => panic!("unexpected submit error: {error}"),
                    }
                }
            })
            .await
            .expect("queue drained")
            .expect("sentinel completes");
            assert!(
                !file.exists(),
                "the queued pty program ran after its call was dropped"
            );
            let _ = std::fs::remove_file(&file);
            drop(blocker);
        });
    }

    #[test]
    fn a_running_child_is_held_back_by_a_full_buffer_and_nothing_else_is() {
        use super::{read_room, OUTPUT_LIMIT, READ_CHUNK};
        assert_eq!(read_room(0, false, false, false), READ_CHUNK);
        assert_eq!(read_room(OUTPUT_LIMIT - 10, false, false, false), 10);
        assert_eq!(read_room(OUTPUT_LIMIT, false, false, false), 0);
        assert_eq!(read_room(OUTPUT_LIMIT + 1, false, false, false), 0);
        for (exited, shutdown, page_gone) in [
            (true, false, false),
            (false, true, false),
            (false, false, true),
        ] {
            assert_eq!(
                read_room(OUTPUT_LIMIT, exited, shutdown, page_gone),
                READ_CHUNK
            );
        }
    }

    #[test]
    fn pty_stalled_output_is_bounded_and_reports_gap_before_end_or_error() {
        for failed in [false, true] {
            let mut buffer = super::OutputBuffer::default();
            for _ in 0..1000 {
                buffer.push(&vec![b'x'; super::READ_CHUNK]);
                assert!(buffer.length <= super::OUTPUT_LIMIT);
                assert!(buffer.chunks.len() <= super::OUTPUT_LIMIT / super::READ_CHUNK);
            }
            buffer.done = true;
            if failed {
                buffer.error = Some(super::super::invalid("read failed"));
            }
            let mut retained = 0;
            for _ in 0..super::OUTPUT_LIMIT / super::READ_CHUNK {
                match buffer.pop() {
                    Some(super::OutputItem::Bytes(bytes)) => {
                        retained += bytes.len();
                        assert!(bytes.iter().all(|b| *b == b'x'));
                    }
                    _ => panic!("retained prefix before overflow"),
                }
            }
            assert_eq!(retained, super::OUTPUT_LIMIT);
            assert!(
                matches!(buffer.pop(), Some(super::OutputItem::Overflow(n)) if n == (1000 * super::READ_CHUNK - retained) as u64)
            );
            match buffer.pop() {
                Some(super::OutputItem::Error(error)) if failed => {
                    assert_eq!(error.message, "read failed")
                }
                Some(super::OutputItem::End) if !failed => {}
                _ => panic!("lifecycle after overflow"),
            }
        }
        let mut buffer = super::OutputBuffer::default();
        buffer.push(&vec![b'a'; super::OUTPUT_LIMIT]);
        buffer.push(b"lost");
        assert!(matches!(buffer.pop(), Some(super::OutputItem::Bytes(_))));
        buffer.push(b"also lost"); // Do not insert bytes in an unreported gap.
        assert!(matches!(
            buffer.pop(),
            Some(super::OutputItem::Overflow(13))
        ));
        buffer.push(b"tail");
        buffer.done = true;
        match buffer.pop() {
            Some(super::OutputItem::Bytes(bytes)) => assert_eq!(&bytes[..], b"tail"),
            _ => panic!("tail retained after gap"),
        }
        assert!(matches!(buffer.pop(), Some(super::OutputItem::End)));
    }
}

const READ_CHUNK: usize = 32 * 1024;
const OUTPUT_LIMIT: usize = 256 * 1024;

/// How much the pump may read now. A running child is held back (backpressure) once the buffer
/// is full; after it has exited, or when the page or the session is gone, ConPTY is drained
/// whatever the credit, and what does not fit is counted as lost.
fn read_room(held: usize, exited: bool, shutdown: bool, page_gone: bool) -> usize {
    if exited || shutdown || page_gone {
        READ_CHUNK
    } else {
        OUTPUT_LIMIT.saturating_sub(held).min(READ_CHUNK)
    }
}

/// Retain the prefix, coalesce its following gap, then resume after reporting that gap.
/// A gap never overtakes retained bytes; neither EOF nor an error overtakes the gap.
#[derive(Default)]
struct OutputBuffer {
    chunks: std::collections::VecDeque<bytes::Bytes>,
    length: usize,
    dropped: u64,
    done: bool,
    error: Option<AlefError>,
}
enum OutputItem {
    Bytes(bytes::Bytes),
    Overflow(u64),
    End,
    Error(AlefError),
}
impl OutputBuffer {
    fn push(&mut self, bytes: &[u8]) {
        let keep = if self.dropped == 0 {
            bytes.len().min(OUTPUT_LIMIT - self.length)
        } else {
            0
        };
        if keep > 0 {
            self.chunks
                .push_back(bytes::Bytes::copy_from_slice(&bytes[..keep]));
            self.length += keep;
        }
        self.dropped = self.dropped.saturating_add((bytes.len() - keep) as u64);
    }
    fn pop(&mut self) -> Option<OutputItem> {
        if let Some(bytes) = self.chunks.pop_front() {
            self.length -= bytes.len();
            return Some(OutputItem::Bytes(bytes));
        }
        if self.dropped > 0 {
            return Some(OutputItem::Overflow(std::mem::take(&mut self.dropped)));
        }
        if self.done {
            return Some(match self.error.take() {
                Some(error) => OutputItem::Error(error),
                None => OutputItem::End,
            });
        }
        None
    }
}

pub(crate) struct Terminal(platform::Terminal);
impl Terminal {
    pub(crate) fn stop(&self) {
        self.0.stop();
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {
    program: String,
    #[serde(default)]
    args: Vec<String>,
    cols: u16,
    rows: u16,
    cwd: Option<String>,
    env: Option<Vec<(String, String)>>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Resize {
    process: u64,
    cols: u16,
    rows: u16,
}

fn size(cols: u16, rows: u16) -> Result<(), AlefError> {
    if !(1..=1000).contains(&cols) || !(1..=1000).contains(&rows) {
        return Err(super::invalid(
            "terminal dimensions must be integers in 1..1000",
        ));
    }
    Ok(())
}

pub(crate) fn register(registry: &mut Registry) -> Result<(), AlefError> {
    registry
        .command::<Args>("cli.pty")?
        .permission(Permission::CliExec, |args| Some(args.program.clone()))
        .substitutes()
        .handler(|ctx, args| async move {
            if ctx.decision() == Decision::Substitute {
                return Err(super::dead(None).await);
            }
            size(args.cols, args.rows)?;
            let env = super::spawn::check_env(args.env, super::wildcard(&ctx))?;
            let cwd = super::cwd_of(&ctx, &args.cwd).await?;
            let program = super::resolve_program(args.program.clone()).await?;
            crate::json(
                &open(
                    super::executor(),
                    &ctx.session,
                    program,
                    args.args,
                    cwd,
                    env,
                    args.cols,
                    args.rows,
                )
                .await?,
            )
        })?;
    registry
        .command::<Resize>("cli.resize")?
        .handler(|ctx, args| async move {
            size(args.cols, args.rows)?;
            let terminal =
                ctx.resources()
                    .with_as::<Process, _>(ResourceId(args.process), |process| {
                        if process
                            .shared
                            .claimed
                            .load(std::sync::atomic::Ordering::Acquire)
                        {
                            return Err(AlefError::new(
                                ErrorCode::NotFound,
                                "process already waited for",
                            ));
                        }
                        process
                            .terminal
                            .clone()
                            .ok_or_else(|| super::invalid("process is not a terminal"))
                    })??;
            terminal.0.resize(args.cols, args.rows)?;
            Ok(Reply::Json(Value::Null))
        })
}

/// cancel-safe: yes — the pty creation runs on the blocking executor: dropping the job before it
/// starts skips the open, a started open cannot be stopped and its result is dropped by the job;
/// installation transfers all ownership before yielding.
async fn open(
    executor: &BlockingExecutor,
    session: &Arc<Session>,
    program: PathBuf,
    args: Vec<String>,
    cwd: PathBuf,
    env: Vec<(String, String)>,
    cols: u16,
    rows: u16,
) -> Result<Value, AlefError> {
    let (terminal, child, killer, pid) = executor
        .submit(move || platform::open(program, args, cwd, env, cols, rows))?
        .result()
        .await??;
    let terminal = Arc::new(Terminal(terminal));
    let (result_tx, result) = tokio::sync::watch::channel(None);
    let shared = Arc::new(Shared {
        result,
        killer,
        claimed: std::sync::atomic::AtomicBool::new(false),
        claimed_notify: tokio::sync::Notify::new(),
        shutdown: tokio::sync::watch::channel(false).0,
    });
    let process = Process {
        shared: shared.clone(),
        tasks: Mutex::new(Vec::new()),
        terminal: Some(terminal.clone()),
    };
    let id = match session.resources().insert(Box::new(process)) {
        Ok(id) => id,
        Err(error) => {
            shared.killer.kill();
            // fire-and-forget: failed insertion still owns reaping and console draining.
            tokio::spawn(async move {
                let _ = child.wait().await;
                let drain = terminal.clone();
                let pump = tokio::spawn(async move {
                    let mut buffer = vec![0; 32 * 1024];
                    loop {
                        match drain.0.read(&mut buffer) {
                            Ok(Some(0)) | Err(_) => break,
                            Ok(None) => tokio::time::sleep(Duration::from_millis(5)).await,
                            Ok(Some(_)) => {}
                        }
                    }
                });
                terminal.0.finished().await;
                let _ = pump.await;
            });
            return Err(error);
        }
    };
    let (output, output_id) = session.streams().open_outgoing();
    let (mut input, input_id) = session.streams().open_incoming_reader();
    let buffer = Arc::new(Mutex::new(OutputBuffer::default()));
    let (changed_tx, mut changed) = tokio::sync::watch::channel(());
    let (exit_tx, mut exited) = tokio::sync::watch::channel(false);
    let (consumed_tx, mut consumed) = tokio::sync::watch::channel(());
    session.resources().with_as::<Process, _>(id, |process| {
        let mut pumps = tokio::task::JoinSet::new();
        let read = terminal.clone();
        let mut read_shutdown = shared.shutdown.subscribe();
        let mut capture_shutdown = shared.shutdown.subscribe();
        let capture = buffer.clone();
        pumps.spawn(async move {
            let mut bytes = vec![0; READ_CHUNK];
            loop {
                let available = loop {
                    let held = capture.lock().unwrap_or_else(|e| e.into_inner()).length;
                    let room = read_room(
                        held,
                        *exited.borrow(),
                        *capture_shutdown.borrow(),
                        consumed.has_changed().is_err(),
                    );
                    if room > 0 {
                        break room;
                    }
                    tokio::select! {
                        _ = consumed.changed() => {},
                        _ = exited.changed() => {},
                        _ = capture_shutdown.changed() => {},
                    }
                };
                match read.0.read(&mut bytes[..available]) {
                    Ok(Some(0)) => break,
                    Ok(Some(n)) => {
                        if *capture_shutdown.borrow() || consumed.has_changed().is_err() {
                            continue;
                        }
                        capture.lock().unwrap_or_else(|e| e.into_inner()).push(&bytes[..n]);
                        changed_tx.send_replace(());
                    }
                    Ok(None) => tokio::time::sleep(Duration::from_millis(5)).await,
                    Err(error) => {
                        read.0.abandon_output();
                        capture.lock().unwrap_or_else(|e| e.into_inner()).error = Some(error);
                        break;
                    }
                }
            }
            capture.lock().unwrap_or_else(|e| e.into_inner()).done = true;
            changed_tx.send_replace(());
        });
        let mut replay = tokio::task::JoinSet::new();
        replay.spawn(async move {
            loop {
                changed.borrow_and_update();
                let item = buffer.lock().unwrap_or_else(|e| e.into_inner()).pop();
                consumed_tx.send_replace(());
                match item {
                    Some(OutputItem::Bytes(bytes)) => {
                        tokio::select! {
                            result = output.send_binary(bytes) => if result.is_err() { return; },
                            _ = read_shutdown.changed() => return,
                        }
                    }
                    Some(OutputItem::Overflow(dropped)) => {
                        tokio::select! {
                            result = output.send_json(json!({"kind":"overflow", "dropped":dropped})) => if result.is_err() { return; },
                            _ = read_shutdown.changed() => return,
                        }
                    }
                    Some(OutputItem::End) => { output.end(); return; }
                    Some(OutputItem::Error(error)) => { output.error(error); return; }
                    None => {
                        tokio::select! {
                            result = changed.changed() => if result.is_err() { return; },
                            _ = read_shutdown.changed() => return,
                        }
                    }
                }
            }
        });

        let write = terminal.clone();
        let mut inputs = tokio::task::JoinSet::new();
        inputs.spawn(async move {
            while let Some(Ok(bytes)) = input.recv().await {
                if write.0.write(bytes).await.is_err() { break; }
            }
            write.0.end_input();
        });
        let shared = shared.clone();
        let session = Arc::downgrade(session);
        let task = tokio::spawn(async move {
            let status = child.wait().await;
            exit_tx.send_replace(true);
            inputs.abort_all();
            while inputs.join_next().await.is_some() {}
            result_tx.send_replace(Some(status));
            terminal.0.finished().await;
            while pumps.join_next().await.is_some() {}
            let mut pumps = replay;
            let mut shutdown = shared.shutdown.subscribe();
            loop {
                if *shutdown.borrow() { pumps.abort_all(); }
                tokio::select! {
                    next = pumps.join_next() => if next.is_none() { break; },
                    _ = shutdown.changed() => pumps.abort_all(),
                }
            }
            if !shared.claimed.load(std::sync::atomic::Ordering::Acquire) && !*shutdown.borrow() {
                tokio::select! { _ = shared.claimed_notify.notified() => {}, _ = shutdown.changed() => {} }
            }
            if let Some(session) = session.upgrade() { let _ = session.resources().remove(id); }
        });
        process.tasks.lock().unwrap_or_else(|e| e.into_inner()).push(task);
    })?;
    Ok(json!({"process":id.0,"pid":pid,"output":output_id.0,"input":input_id.0}))
}
