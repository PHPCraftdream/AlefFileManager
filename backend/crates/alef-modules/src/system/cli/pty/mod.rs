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

use super::spawn::{Process, Shared};
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
            let cwd = super::cwd_of(&ctx, &args.cwd)?;
            let program = super::tree::resolve(
                &args.program,
                std::env::var_os("PATH").as_deref(),
                std::env::var_os("PATHEXT").as_deref(),
            )
            .ok_or_else(|| AlefError::new(ErrorCode::NotFound, "program not found"))?;
            crate::json(
                &open(
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

/// cancel-safe: yes — setup has no await; installation transfers all ownership before yielding.
async fn open(
    session: &Arc<Session>,
    program: PathBuf,
    args: Vec<String>,
    cwd: PathBuf,
    env: Vec<(String, String)>,
    cols: u16,
    rows: u16,
) -> Result<Value, AlefError> {
    let (terminal, child, killer, pid) = platform::open(program, args, cwd, env, cols, rows)?;
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
    let spool_owner = tempfile::NamedTempFile::new()?;
    let spool = spool_owner.reopen()?;
    let spool_read = spool_owner.reopen()?;
    let (spool_tx, spool_rx) = tokio::sync::watch::channel((0_u64, false));
    let (exit_tx, mut exited) = tokio::sync::watch::channel(false);
    let (consumed_tx, mut consumed) = tokio::sync::watch::channel(0_u64);
    session.resources().with_as::<Process, _>(id, |process| {
        let mut pumps = tokio::task::JoinSet::new();
        let read = terminal.clone();
        let mut read_shutdown = shared.shutdown.subscribe();
        let mut spool = tokio::fs::File::from_std(spool);
        let mut capture_shutdown = shared.shutdown.subscribe();
        pumps.spawn(async move {
            use tokio::io::{AsyncSeekExt, AsyncWriteExt};
            let mut length = 0_u64;
            let mut buffer = vec![0; 32 * 1024];
            loop {
                // While running, retain at most one credit window on disk. After exit the
                // OS terminal buffer is finite; drain it regardless of page credit.
                let discard = *capture_shutdown.borrow() || consumed.has_changed().is_err();
                while !discard && length.saturating_sub(*consumed.borrow()) >= if *exited.borrow() { 32 * 1024 * 1024 - 32768 } else { 256 * 1024 } {
                    tokio::select! { _ = consumed.changed() => {}, _ = exited.changed() => {}, _ = capture_shutdown.changed() => {} }
                    if *capture_shutdown.borrow() || consumed.has_changed().is_err() { break; }
                }
                let at = length % (32 * 1024 * 1024);
                let available = ((32 * 1024 * 1024 - at) as usize).min(buffer.len());
                match read.0.read(&mut buffer[..available]) {
                    Ok(Some(0)) => break,
                    Ok(Some(n)) => {
                        if *capture_shutdown.borrow() || consumed.has_changed().is_err() { continue; }
                        if let Err(error) = spool.seek(std::io::SeekFrom::Start(at)).await {
                            read.0.abandon_output();
                            return Err(alef_core::AlefError::from(error));
                        }
                        if let Err(error) = spool.write_all(&buffer[..n]).await {
                            read.0.abandon_output();
                            return Err(alef_core::AlefError::from(error));
                        }
                        if let Err(error) = spool.flush().await {
                            read.0.abandon_output();
                            return Err(alef_core::AlefError::from(error));
                        }
                        length += n as u64;
                        spool_tx.send_replace((length, false));
                    }
                    Ok(None) => tokio::time::sleep(Duration::from_millis(5)).await,
                    Err(error) => { read.0.abandon_output(); return Err(error); }
                }
            }
            spool_tx.send_replace((length, true));
            Ok(())
        });
        let mut replay = tokio::task::JoinSet::new();
        replay.spawn(async move {
            let _spool_owner = spool_owner;
            use tokio::io::{AsyncReadExt, AsyncSeekExt};
            let mut file = tokio::fs::File::from_std(spool_read);
            let mut state = spool_rx;
            let mut offset = 0;
            let mut buffer = vec![0; 32 * 1024];
            loop {
                let (length, done) = *state.borrow_and_update();
                if offset < length {
                    let at = offset % (32 * 1024 * 1024);
                    let n = ((length - offset).min(32 * 1024 * 1024 - at) as usize).min(buffer.len());
                    // Reopened handles have independent cursors; the ring never overwrites unread bytes.
                    if file.seek(std::io::SeekFrom::Start(at)).await.is_err() { break; }
                    if let Err(error) = file.read_exact(&mut buffer[..n]).await { output.error(error.into()); return; }
                    tokio::select! {
                        result = output.send_binary(bytes::Bytes::copy_from_slice(&buffer[..n])) => if result.is_err() { return; },
                        _ = read_shutdown.changed() => return,
                    }
                    offset += n as u64;
                    consumed_tx.send_replace(offset);
                } else if done { output.end(); return; }
                else if state.changed().await.is_err() { output.error(super::invalid("terminal output spool failed")); return; }
            }
            output.error(super::invalid("terminal output spool seek failed"));
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
