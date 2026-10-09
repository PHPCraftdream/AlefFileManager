// SPDX-License-Identifier: MIT OR Apache-2.0
//! A process that stays: its pipes go to streams of the page (with credit, both ways), the resource
//! of it can be waited for once and killed, and killing takes the whole tree.
use std::{
    path::PathBuf,
    process::Stdio,
    sync::{Arc, Mutex},
};

use alef_core::{
    session::{
        resources::Resource,
        session::Session,
        streams::{IncomingReader, StreamWriter},
    },
    AlefError, ErrorCode,
};
use bytes::Bytes;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    process::Command,
    task::JoinHandle,
};

#[cfg(unix)]
use super::exec;
#[cfg(windows)]
use super::native::Child;
use super::tree::Killer;
#[cfg(unix)]
use tokio::process::Child;

/// How many bytes one read of a pipe of the child takes at the most.
const READ_CHUNK: usize = 32 * 1024;

/// What a pipe of the child is: wired to a stream of the page, or going nowhere.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub(crate) enum PipeArg {
    Pipe,
    Ignore,
}

/// Which pipes of the child are wired to streams of the page.
#[derive(Clone, Copy)]
pub(crate) struct Pipes {
    pub stdin: bool,
    pub stdout: bool,
    pub stderr: bool,
}

/// Whether the pipe is wired.
pub(crate) fn piped(arg: Option<PipeArg>) -> bool {
    arg != Some(PipeArg::Ignore)
}

pub(crate) fn pipes_of(
    stdin: Option<PipeArg>,
    stdout: Option<PipeArg>,
    stderr: Option<PipeArg>,
) -> Pipes {
    Pipes {
        stdin: piped(stdin),
        stdout: piped(stdout),
        stderr: piped(stderr),
    }
}

fn closed(message: String) -> AlefError {
    AlefError::new(ErrorCode::Closed, message)
}

/// Whether the name of an environment variable may not be given: the ones that steer how a program
/// is found and started, and the dynamic linkers' own.
fn refused_env(name: &str) -> bool {
    let path_like = ["PATH", "PATHEXT", "COMSPEC"].iter().any(|banned| {
        if cfg!(windows) {
            banned.eq_ignore_ascii_case(name)
        } else {
            *banned == name
        }
    });
    path_like || name.starts_with("LD_") || name.starts_with("DYLD_")
}

/// The pairs the page may add to the environment of a child: none of the refused ones, unless the
/// right is `*`.
pub(crate) fn check_env(
    env: Option<Vec<(String, String)>>,
    wildcard: bool,
) -> Result<Vec<(String, String)>, AlefError> {
    let env = env.unwrap_or_default();
    if env
        .iter()
        .any(|(name, value)| name.is_empty() || name.contains(['\0', '=']) || value.contains('\0'))
    {
        return Err(AlefError::new(
            ErrorCode::InvalidArgument,
            "malformed environment pair",
        ));
    }
    if !wildcard {
        if let Some((name, _)) = env.iter().find(|(name, _)| refused_env(name)) {
            return Err(AlefError::new(
                ErrorCode::InvalidArgument,
                format!("the environment variable {name} needs permissions.cli.exec: [*]"),
            ));
        }
    }
    Ok(env)
}

/// Sends what the child writes to the page, as the page takes it; the end of the pipe is the end of
/// the stream.
async fn read_pump<R: AsyncRead + Unpin>(mut half: R, writer: StreamWriter) {
    let mut buffer = vec![0_u8; READ_CHUNK];
    loop {
        match half.read(&mut buffer).await {
            Ok(0) => {
                writer.end();
                return;
            }
            Ok(count) => {
                if writer
                    .send_binary(Bytes::copy_from_slice(&buffer[..count]))
                    .await
                    .is_err()
                {
                    return;
                }
            }
            Err(error) => {
                writer.error(closed(format!("the pipe of the process broke: {error}")));
                return;
            }
        }
    }
}

/// Writes what the page sends to the child; the end of the stream closes the input of the child.
async fn write_pump<W: AsyncWrite + Unpin>(mut half: W, mut reader: IncomingReader) {
    while let Some(Ok(bytes)) = reader.recv().await {
        if half.write_all(&bytes).await.is_err() {
            return;
        }
    }
    // Dropping the pipe closes the input of the child.
}

/// The child and its tree-killer, shared by the resource and the wait.
pub(crate) struct Shared {
    pub(crate) result:
        tokio::sync::watch::Receiver<Option<Result<std::process::ExitStatus, AlefError>>>,
    pub killer: Killer,
    pub claimed: std::sync::atomic::AtomicBool,
    pub(crate) claimed_notify: tokio::sync::Notify,
    pub(crate) shutdown: tokio::sync::watch::Sender<bool>,
}

/// The session owns the supervisor until both output pumps have reached EOF.
/// A completed wait does not cancel pumps blocked on the page's credit.
pub(crate) struct Process {
    pub shared: Arc<Shared>,
    pub(crate) tasks: Mutex<Vec<JoinHandle<()>>>,
    pub(crate) terminal: Option<Arc<super::pty::Terminal>>,
}

impl Drop for Process {
    fn drop(&mut self) {
        self.shared.killer.kill();
        if let Some(terminal) = &self.terminal {
            terminal.stop();
            self.shared.shutdown.send_replace(true);
            // fire-and-forget: terminal supervisor finishes draining ConPTY on resource drop.
            return;
        }
        for task in self.tasks.get_mut().unwrap_or_else(|e| e.into_inner()) {
            task.abort();
        }
    }
}

impl Resource for Process {
    fn close(self: Box<Self>) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
        Box::pin(async move {
            self.shared.killer.kill();
            if let Some(terminal) = &self.terminal {
                terminal.stop();
            }
            self.shared.shutdown.send_replace(true);
            // The supervisor reaps the child and observes stream shutdown before returning.
            let tasks = std::mem::take(&mut *self.tasks.lock().unwrap_or_else(|e| e.into_inner()));
            for task in tasks {
                let _ = task.await;
            }
        })
    }
}

/// cancel-safe: yes — observing exit does not consume the session resource;
/// the once-only claim happens after exit with no intervening await.
pub(crate) async fn wait(shared: Arc<Shared>) -> Result<(Option<i32>, Option<String>), AlefError> {
    let mut result = shared.result.clone();
    let status = loop {
        if let Some(status) = result.borrow().clone() {
            break status;
        }
        result
            .changed()
            .await
            .map_err(|_| AlefError::new(ErrorCode::Closed, "process supervisor closed"))?;
    };
    if shared
        .claimed
        .swap(true, std::sync::atomic::Ordering::AcqRel)
    {
        return Err(AlefError::new(
            ErrorCode::NotFound,
            "process already waited for",
        ));
    }
    shared.claimed_notify.notify_one();
    let status = status?;
    #[cfg(unix)]
    let signal = std::os::unix::process::ExitStatusExt::signal(&status).map(exec::signal_name);
    #[cfg(windows)]
    let signal = None;
    Ok((status.code(), signal))
}

/// Owns pump tasks even when the supervisor is aborted (JoinSet aborts on drop).
async fn supervise(
    mut child: Child,
    shared: Arc<Shared>,
    result: tokio::sync::watch::Sender<Option<Result<std::process::ExitStatus, AlefError>>>,
    mut pumps: tokio::task::JoinSet<()>,
    mut input: tokio::task::JoinSet<()>,
    session: std::sync::Weak<Session>,
    id: alef_core::ids::ResourceId,
) {
    let status = child.wait().await.map_err(AlefError::from);
    if status.is_err() {
        shared.killer.kill();
        let _ = child.kill().await;
        let _ = child.wait().await;
    }
    input.abort_all();
    while input.join_next().await.is_some() {}
    shared.killer.disarm(); // kills remaining descendants before relinquishing the group
    let failed = status.is_err();
    result.send_replace(Some(status));
    if failed || session.upgrade().is_none_or(|s| s.resources().is_closed()) {
        pumps.abort_all();
    }
    let mut shutdown = shared.shutdown.subscribe();
    loop {
        if *shutdown.borrow() {
            pumps.abort_all();
        }
        tokio::select! {
            item = pumps.join_next() => if item.is_none() { break; },
            _ = shutdown.changed() => { pumps.abort_all(); }
        }
    }
    if !*shutdown.borrow() {
        tokio::select! {
            _ = shared.claimed_notify.notified() => {},
            _ = shutdown.changed() => {},
        }
    }
    if let Some(session) = session.upgrade() {
        let _ = session.resources().remove(id);
    }
}

/// Starts a process with its pipes wired to streams, and answers with the ids of all of them.
pub(crate) async fn spawn_command(
    session: &Arc<Session>,
    program: PathBuf,
    args: Vec<String>,
    cwd: PathBuf,
    env: Vec<(String, String)>,
    pipes: Pipes,
) -> Result<Value, AlefError> {
    let Pipes {
        stdin: stdin_piped,
        stdout: stdout_piped,
        stderr: stderr_piped,
    } = pipes;
    let mut command = Command::new(&program);
    command
        .args(&args)
        .current_dir(&cwd)
        .stdout(if stdout_piped {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stderr(if stderr_piped {
            Stdio::piped()
        } else {
            Stdio::null()
        })
        .stdin(if stdin_piped {
            Stdio::piped()
        } else {
            Stdio::null()
        });
    for (name, value) in &env {
        command.env(name, value);
    }
    #[cfg(unix)]
    let (mut child, killer) = super::tree::spawn(&mut command).await?;
    #[cfg(windows)]
    let (mut child, killer) = super::native::spawn(&program, &args, &cwd, &env, pipes)?;
    let mut tasks = tokio::task::JoinSet::new();
    let mut input = tokio::task::JoinSet::new();
    let (stdin, stdout, stderr) = (
        if stdin_piped {
            child.stdin.take()
        } else {
            None
        },
        if stdout_piped {
            child.stdout.take()
        } else {
            None
        },
        if stderr_piped {
            child.stderr.take()
        } else {
            None
        },
    );
    let mut stdin_id = None;
    if let Some(half) = stdin {
        let (reader, write) = session.streams().open_incoming_reader();
        stdin_id = Some(write.0);
        input.spawn(write_pump(half, reader));
    }
    let mut stdout_id = None;
    if let Some(half) = stdout {
        let (writer, read) = session.streams().open_outgoing();
        stdout_id = Some(read.0);
        tasks.spawn(read_pump(half, writer));
    }
    let mut stderr_id = None;
    if let Some(half) = stderr {
        let (writer, read) = session.streams().open_outgoing();
        stderr_id = Some(read.0);
        tasks.spawn(read_pump(half, writer));
    }
    let pid = child.id();
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
        terminal: None,
    };
    let id = match session.resources().insert(Box::new(process)) {
        Ok(id) => id,
        Err(error) => {
            shared.killer.kill();
            let _ = child.wait().await;
            input.abort_all();
            while input.join_next().await.is_some() {}
            tasks.abort_all();
            while tasks.join_next().await.is_some() {}
            return Err(error);
        }
    };
    // Insertion and supervisor ownership transfer have no await/cancellation boundary.
    let installed = session.resources().with_as::<Process, _>(id, |process| {
        let worker = tokio::spawn(supervise(
            child,
            shared.clone(),
            result_tx,
            tasks,
            input,
            Arc::downgrade(session),
            id,
        ));
        process
            .tasks
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .push(worker);
    });
    if let Err(error) = installed {
        shared.killer.kill();
        return Err(error);
    }
    Ok(json!({
        "process": id.0,
        "pid": pid,
        "stdin": stdin_id,
        "stdout": stdout_id,
        "stderr": stderr_id,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_search_path_variables_are_refused_and_others_are_not() {
        assert!(refused_env("PATH"));
        assert!(refused_env("PATHEXT"));
        assert!(refused_env("COMSPEC"));
        assert_eq!(refused_env("Path"), cfg!(windows));
        assert!(!refused_env("ALEF_X"));
        assert!(refused_env("LD_PRELOAD"));
        assert!(refused_env("DYLD_X"));
        assert!(!refused_env("MY_LD_X"));
    }

    #[test]
    fn a_refused_pair_is_an_error_naming_the_variable() {
        let error = check_env(Some(vec![("PATH".to_owned(), "x".to_owned())]), false).unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidArgument);
        assert!(error.message.contains("PATH"));
        assert!(check_env(Some(vec![("PATH".to_owned(), "x".to_owned())]), true).is_ok());
        assert!(check_env(None, false).unwrap().is_empty());
        for (name, value) in [("", "x"), ("A=B", "x"), ("A\0", "x"), ("A", "x\0y")] {
            let pair = Some(vec![(name.to_owned(), value.to_owned())]);
            let error = check_env(pair, true).unwrap_err();
            assert_eq!(error.code, ErrorCode::InvalidArgument, "{name:?}={value:?}");
        }
    }
}
