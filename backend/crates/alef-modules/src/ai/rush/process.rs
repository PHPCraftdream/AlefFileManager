use std::{
    collections::{HashMap, VecDeque},
    path::{Path, PathBuf},
    time::Duration,
};

use alef_core::{registry::context::CancelHandle, AlefError, ErrorCode};
use serde_json::Value;
use tokio::sync::mpsc;

pub(crate) use super::{ChatControl, ChatEvent};
use crate::ai::CHUNK;
#[derive(Debug, Clone)]
pub struct RushCommand {
    pub program: String,
    pub prefix: Vec<String>,
}
/// The direct-API providers a slot may be served by.
pub(crate) const ALLOWED_PROVIDERS: [&str; 3] = ["openai", "anthropic", "google"];
const MAX_PROBE_OUTPUT: usize = 1024 * 1024;
pub(crate) const PROBE_TIME: Duration = Duration::from_secs(10);
pub(crate) const MAX_SESSIONS: usize = 64;

pub(crate) fn filter_env(inherited: Vec<(String, String)>) -> Vec<(String, String)> {
    inherited
        .into_iter()
        .filter(|(key, _)| !key.to_uppercase().starts_with("ALEF_"))
        .collect()
}
pub(crate) fn filtered_env() -> Vec<(String, String)> {
    filter_env(std::env::vars().collect())
}
pub(crate) fn scratch_dir(root: &Path, label: &str) -> PathBuf {
    root.join(format!(
        "{label}-{}",
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default()
    ))
}
pub(crate) enum ProbeOutcome {
    Json(Value),
    TimedOut,
    Failed,
}

pub(crate) mod sync {
    use super::{filtered_env, scratch_dir, Duration, PathBuf, RushCommand, PROBE_TIME};
    pub(crate) fn resolve(command: &RushCommand) -> Option<PathBuf> {
        crate::system::cli::tree::resolve(
            &command.program,
            std::env::var_os("PATH").as_deref(),
            std::env::var_os("PATHEXT").as_deref(),
        )
    }
    pub(crate) fn spawn(
        command: &RushCommand,
        args: &[String],
        scratch: &std::path::Path,
        capture: bool,
    ) -> Option<std::process::Child> {
        let resolved = resolve(command)?;
        let (stdout, stderr) = if capture {
            (std::process::Stdio::piped(), std::process::Stdio::piped())
        } else {
            (std::process::Stdio::null(), std::process::Stdio::null())
        };
        std::process::Command::new(resolved)
            .args(&command.prefix)
            .args(args)
            .env_clear()
            .envs(filtered_env())
            .current_dir(scratch)
            .stdin(std::process::Stdio::null())
            .stdout(stdout)
            .stderr(stderr)
            .spawn()
            .ok()
    }
    pub(crate) fn bounded_wait(
        child: &mut std::process::Child,
    ) -> Option<std::process::ExitStatus> {
        let started = std::time::Instant::now();
        loop {
            match child.try_wait() {
                Ok(Some(status)) => return Some(status),
                Ok(None) if started.elapsed() < PROBE_TIME => {
                    std::thread::sleep(Duration::from_millis(20))
                }
                _ => {
                    let _ = child.kill();
                    let _ = child.wait();
                    return None;
                }
            }
        }
    }
    pub(crate) fn run(command: &RushCommand, data_root: &std::path::Path, args: &[String]) -> bool {
        let scratch = scratch_dir(data_root, "shot");
        let done = std::fs::create_dir(&scratch).is_ok()
            && spawn(command, args, &scratch, false)
                .and_then(|mut child| bounded_wait(&mut child))
                .is_some_and(|s| s.success());
        let _ = std::fs::remove_dir_all(scratch);
        done
    }
}

pub(crate) fn effective_of(value: &Value) -> Value {
    value.get("effective").cloned().unwrap_or(Value::Null)
}
pub(crate) fn spawnable_program(path: &Path) -> bool {
    if cfg!(windows) {
        path.extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("exe") || ext.eq_ignore_ascii_case("com"))
    } else {
        true
    }
}
pub(crate) async fn run_probe(command: &RushCommand, data_root: &Path) -> ProbeOutcome {
    use tokio::io::AsyncReadExt;
    let started = tokio::time::Instant::now();
    let scratch = scratch_dir(data_root, "probe");
    let root = data_root.to_path_buf();
    let make_dirs = scratch.clone();
    let dirs_job = match crate::ai::builtin::executor().submit(move || {
        std::fs::create_dir_all(&root).is_ok() && std::fs::create_dir(&make_dirs).is_ok()
    }) {
        Ok(job) => job,
        Err(_) => return ProbeOutcome::Failed,
    };
    if !dirs_job.result().await.unwrap_or(false) {
        return ProbeOutcome::Failed;
    }
    let command = command.clone();
    let resolve_command = command.clone();
    let resolve_job =
        match crate::ai::builtin::executor().submit(move || sync::resolve(&resolve_command)) {
            Ok(job) => job,
            Err(_) => {
                let _ = std::fs::remove_dir_all(&scratch);
                return ProbeOutcome::Failed;
            }
        };
    let resolved = match resolve_job.result().await.ok().flatten() {
        Some(path) => path,
        None => {
            let _ = std::fs::remove_dir_all(&scratch);
            return ProbeOutcome::Failed;
        }
    };
    let command = command.clone();
    let scratch_spawn = scratch.clone();
    let argv = vec!["models".into(), "show".into(), "--json".into()];
    let spawn_job = match crate::ai::builtin::executor()
        .submit(move || spawn_child(&command, &resolved, &argv, &scratch_spawn))
    {
        Ok(job) => job,
        Err(_) => {
            let _ = std::fs::remove_dir_all(&scratch);
            return ProbeOutcome::Failed;
        }
    };
    let mut process = match spawn_job.result().await.ok().and_then(Result::ok) {
        Some(process) => process,
        None => {
            let _ = std::fs::remove_dir_all(&scratch);
            return ProbeOutcome::Failed;
        }
    };
    drop(process.stdin.take());
    let mut stdout = process.stdout.take().expect("piped stdout");
    let stderr = process.stderr.take().expect("piped stderr");
    let out = tokio::spawn(async move {
        let mut bytes = Vec::new();
        let mut state = crate::ai::rush::io::BoundedBytes::new(256 * 1024, MAX_PROBE_OUTPUT);
        let mut chunk = vec![0; CHUNK];
        loop {
            match stdout.read(&mut chunk).await {
                Ok(0) | Err(_) => break,
                Ok(n) => {
                    for line in state.push(&chunk[..n]) {
                        bytes.extend_from_slice(&line);
                    }
                }
            }
        }
        if let Some(line) = state.finish() {
            bytes.extend_from_slice(&line);
        }
        let over = state.overflowed();
        (bytes, over)
    });
    let err =
        tokio::spawn(async move { crate::ai::rush::io::drain(stderr, 4 * 1024 * 1024).await });
    let remaining = PROBE_TIME.saturating_sub(started.elapsed());
    let status = tokio::time::timeout(remaining, process.child.wait()).await;
    let outcome = match status {
        Ok(Ok(status)) => {
            process.killer.kill();
            let out_result = tokio::time::timeout(Duration::from_millis(250), out).await;
            let stderr_overflow = match tokio::time::timeout(Duration::from_millis(250), err).await
            {
                Ok(Ok(over)) => over,
                _ => true,
            };
            if status.success() && !stderr_overflow {
                match out_result.ok().and_then(Result::ok) {
                    Some((bytes, false)) => serde_json::from_slice::<Value>(&bytes)
                        .ok()
                        .map(|v| ProbeOutcome::Json(effective_of(&v)))
                        .unwrap_or(ProbeOutcome::Failed),
                    _ => ProbeOutcome::Failed,
                }
            } else {
                ProbeOutcome::Failed
            }
        }
        _ => {
            process.killer.kill();
            let _ = process.child.wait().await;
            let _ = out.await;
            let _ = err.await;
            ProbeOutcome::TimedOut
        }
    };
    let _ = std::fs::remove_dir_all(scratch);
    outcome
}

#[derive(Default)]
pub(crate) struct SessionMap {
    map: HashMap<String, String>,
    order: VecDeque<String>,
}
impl SessionMap {
    pub(crate) fn insert(&mut self, conversation: String, session: String) -> Option<String> {
        if let Some(old) = self.map.get_mut(&conversation) {
            *old = session;
            return None;
        }
        let evicted = if self.order.len() >= MAX_SESSIONS {
            self.order.pop_front().and_then(|old| self.map.remove(&old))
        } else {
            None
        };
        self.order.push_back(conversation.clone());
        self.map.insert(conversation, session);
        evicted
    }
    pub(crate) fn get(&self, key: &str) -> Option<&String> {
        self.map.get(key)
    }
    pub(crate) fn ids(&self, cap: usize) -> Vec<String> {
        self.order
            .iter()
            .take(cap)
            .filter_map(|key| self.map.get(key).cloned())
            .collect()
    }
    #[cfg(test)]
    pub(crate) fn len(&self) -> usize {
        self.map.len()
    }

    pub(crate) fn remove(&mut self, key: &str) -> Option<String> {
        let value = self.map.remove(key);
        if value.is_some() {
            self.order.retain(|k| k != key);
        }
        value
    }
}

#[cfg(test)]
pub(crate) struct BoundedReader<R> {
    inner: R,
    max_line: usize,
    max_total: usize,
    pending: Vec<u8>,
    total: usize,
    over: bool,
    highest: usize,
}
#[cfg(test)]
impl<R: std::io::Read> BoundedReader<R> {
    pub(crate) fn new(inner: R, max_line: usize, max_total: usize) -> Self {
        Self {
            inner,
            max_line,
            max_total,
            pending: Vec::new(),
            total: 0,
            over: false,
            highest: 0,
        }
    }
    pub(crate) fn overflowed(&self) -> bool {
        self.over
    }
    pub(crate) fn max_buffered(&self) -> usize {
        self.highest
    }
    fn append(&mut self, buf: &mut Vec<u8>, byte: u8, start: usize) {
        if self.over || buf.len() - start >= self.max_line || self.total >= self.max_total {
            self.over = true;
            return;
        }
        buf.push(byte);
        self.total += 1;
    }
    pub(crate) fn read_line(&mut self, buf: &mut Vec<u8>) -> std::io::Result<usize> {
        let before = buf.len();
        loop {
            if let Some(at) = self.pending.iter().position(|&b| b == b'\n') {
                let bytes = self.pending.drain(..=at).collect::<Vec<_>>();
                for b in bytes {
                    self.append(buf, b, before);
                }
                return Ok(buf.len() - before);
            }
            let mut chunk = vec![0; CHUNK];
            match self.inner.read(&mut chunk) {
                Ok(0) => {
                    if self.pending.is_empty() {
                        return Ok(0);
                    }
                    let bytes = std::mem::take(&mut self.pending);
                    for b in bytes {
                        self.append(buf, b, before);
                    }
                    return Ok(buf.len() - before);
                }
                Ok(n) => {
                    let room = self.max_line.saturating_sub(self.pending.len());
                    let take = n.min(room);
                    self.pending.extend_from_slice(&chunk[..take]);
                    self.highest = self.highest.max(self.pending.len());
                    if take < n {
                        self.over = true;
                    }
                }
                Err(e) => return Err(e),
            }
        }
    }
    pub(crate) fn read_to_end_capped(&mut self, out: &mut Vec<u8>) -> std::io::Result<()> {
        let mut chunk = vec![0; CHUNK];
        loop {
            match self.inner.read(&mut chunk) {
                Ok(0) => return Ok(()),
                Ok(n) => {
                    let take = n.min(self.max_total.saturating_sub(out.len()));
                    out.extend_from_slice(&chunk[..take]);
                    if take < n {
                        self.over = true;
                    }
                }
                Err(e) => return Err(e),
            }
        }
    }
}

pub(crate) async fn fail_after(
    control: &ChatControl,
    sender: &mpsc::Sender<ChatEvent>,
    code: ErrorCode,
    message: &str,
    deadline: tokio::time::Instant,
) {
    let _ = tokio::select! {_ = control.cancel.cancelled()=>None,sent=tokio::time::timeout_at(deadline,sender.send(ChatEvent::Failed{code,message:message.to_owned()}))=>sent.ok()};
}
pub(crate) struct Process {
    pub(crate) child: Child,
    pub(crate) killer: crate::system::cli::tree::Killer,
    pub(crate) stdin: Option<tokio::process::ChildStdin>,
    pub(crate) stdout: Option<tokio::process::ChildStdout>,
    pub(crate) stderr: Option<tokio::process::ChildStderr>,
}
pub(crate) enum Child {
    #[cfg(unix)]
    Unix(tokio::process::Child),
    #[cfg(windows)]
    Windows(crate::system::cli::native::Child),
}
impl Child {
    pub(crate) async fn wait(&mut self) -> std::io::Result<std::process::ExitStatus> {
        match self {
            #[cfg(unix)]
            Self::Unix(c) => c.wait().await,
            #[cfg(windows)]
            Self::Windows(c) => c.wait().await,
        }
    }
}
pub(crate) fn spawn_child(
    _command: &RushCommand,
    program: &Path,
    argv: &[String],
    scratch: &Path,
) -> Result<Process, ()> {
    #[cfg(unix)]
    {
        let mut cmd = tokio::process::Command::new(program);
        cmd.args(argv).current_dir(scratch).env_clear();
        for (k, v) in filtered_env() {
            cmd.env(k, v);
        }
        cmd.stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::piped())
            .stderr(std::process::Stdio::piped());
        let (mut child, killer) = crate::system::cli::tree::spawn_sync(&mut cmd).map_err(|_| ())?;
        Ok(Process {
            stdin: child.stdin.take(),
            stdout: child.stdout.take(),
            stderr: child.stderr.take(),
            child: Child::Unix(child),
            killer,
        })
    }
    #[cfg(windows)]
    {
        let (mut child, killer) = crate::system::cli::native::spawn(
            program,
            argv,
            scratch,
            &Vec::new(),
            &["ALEF_"],
            crate::system::cli::spawn::Pipes {
                stdin: true,
                stdout: true,
                stderr: true,
            },
        )
        .map_err(|_| ())?;
        Ok(Process {
            stdin: child.stdin.take(),
            stdout: child.stdout.take(),
            stderr: child.stderr.take(),
            child: Child::Windows(child),
            killer,
        })
    }
}

fn _cancel_handle_type_check(_: CancelHandle) {}
#[allow(dead_code)]
fn _error_type(_: AlefError) {}
