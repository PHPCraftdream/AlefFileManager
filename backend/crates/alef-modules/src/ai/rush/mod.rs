// SPDX-License-Identifier: MIT OR Apache-2.0
//! Rush CLI bridge.
use std::{
    future::Future,
    path::PathBuf,
    pin::Pin,
    sync::{atomic::AtomicU64, Arc, Mutex},
    time::Duration,
};

use alef_core::{AlefError, ErrorCode};
use serde_json::Value;
use tokio::sync::mpsc;

pub use crate::ai::provider::RushCommand;
use crate::ai::provider::{
    run_probe, sync, ChatControl, ChatEvent, ChatRequest, ProbeOutcome, Role, SessionMap,
};
use crate::ai::{builtin, limits, served_slots, ChatTask};
use crate::system::cli::tree;

mod chat;
pub(crate) mod io;
pub(crate) use chat::fresh_session_id;

/// Max tokens per chat.
const MAX_TOKENS: u64 = 8192;
/// Stdout line cap.
const MAX_LINE: usize = 256 * 1024;
/// Output cap.
const MAX_OUTPUT: usize = 8 * 1024 * 1024;
/// Stderr limit.
const STDERR_CAP: usize = 4 * 1024 * 1024;
const ROLES_CACHE_TTL: Duration = Duration::from_secs(60);
const DID_NOT_START: &str = "the rush run did not start";
const TOO_LATE: &str = "the rush run took too long";
const SEND_GRACE: Duration = Duration::from_secs(1);

type Sessions = Arc<Mutex<SessionMap>>;

/// Session counter.
static SESSION_COUNTER: AtomicU64 = AtomicU64::new(0);

#[cfg(windows)]
fn is_reparse_point(meta: &std::fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    meta.file_attributes() & 0x400 != 0
}
#[cfg(not(windows))]
fn is_reparse_point(_: &std::fs::Metadata) -> bool {
    false
}
pub(crate) struct ProviderRoot {
    parent: PathBuf,
    /// Path to unique parent container and exact `.rush` data child.
    pub(crate) path: tokio::sync::OnceCell<(PathBuf, PathBuf)>,
}

impl ProviderRoot {
    fn new(parent: PathBuf) -> Self {
        Self {
            parent,
            path: tokio::sync::OnceCell::new(),
        }
    }

    pub(crate) async fn get(self: &Arc<Self>) -> Result<PathBuf, AlefError> {
        let owner = self.clone();
        tokio::spawn(async move { owner.get_owned().await })
            .await
            .map_err(|_| {
                AlefError::new(
                    ErrorCode::Internal,
                    "the rush data directory is unavailable",
                )
            })?
    }

    async fn get_owned(&self) -> Result<PathBuf, AlefError> {
        let parent = self.parent.clone();
        let pair = self
            .path
            .get_or_try_init(|| async move {
                let job = builtin::executor()
                    .submit(move || {
                        let mut current = PathBuf::new();
                        let mut components = parent.components().peekable();
                        while let Some(component) = components.next() {
                            current.push(component);
                            let named = components.peek().is_none();
                            match std::fs::symlink_metadata(&current) {
                                Ok(meta)
                                    if named
                                        && (meta.file_type().is_symlink()
                                            || is_reparse_point(&meta)) =>
                                {
                                    return Err(());
                                }
                                // Links above the named directory are the system's (macOS /var).
                                Ok(_) => {
                                    if !std::fs::metadata(&current).is_ok_and(|meta| meta.is_dir())
                                    {
                                        return Err(());
                                    }
                                }
                                Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                                    std::fs::create_dir(&current).map_err(|_| ())?
                                }
                                Err(_) => return Err(()),
                            }
                        }
                        let unique = tempfile::Builder::new()
                            .prefix("alef-provider-")
                            .tempdir_in(&parent)
                            .map_err(|_| ())?;
                        let container = unique.keep();
                        let data = container.join(".rush");
                        std::fs::create_dir(&data).map_err(|_| ())?;
                        Ok((container, data))
                    })
                    .map_err(|_| AlefError::new(ErrorCode::Busy, "the rush executor is busy"))?;
                job.result()
                    .await
                    .map_err(|_| {
                        AlefError::new(
                            ErrorCode::Internal,
                            "the rush data directory is unavailable",
                        )
                    })?
                    .map_err(|_| {
                        AlefError::new(
                            ErrorCode::Internal,
                            "the rush data directory is unavailable",
                        )
                    })
            })
            .await?;
        Ok(pair.1.clone())
    }
}

impl Drop for ProviderRoot {
    fn drop(&mut self) {
        if let Some((container, _data)) = self.path.get_mut() {
            let container = container.clone();
            self.path.take();
            let cleanup = move || {
                let _ = std::fs::remove_dir_all(container);
            };
            let executor = builtin::executor().clone();
            if let Ok(handle) = tokio::runtime::Handle::try_current() {
                handle.spawn(async move {
                    if let Ok(job) = executor.submit(cleanup) {
                        let _ = job.result().await;
                    }
                });
            } else {
                let _ = std::thread::Builder::new()
                    .name("alef-rush-root-cleanup".to_owned())
                    .spawn(cleanup);
            }
        }
    }
}

pub struct RushCliProvider {
    command: RushCommand,
    data_root: Arc<ProviderRoot>,
    chat_deadline: Duration,
    envelope: bool,
    sessions: Sessions,
    roles_cache: Mutex<Option<(std::time::Instant, Vec<Role>)>>,
}

impl RushCliProvider {
    /// Finds rush on PATH.
    pub fn detect() -> Option<Self> {
        let names: &[&str] = if cfg!(windows) {
            &["rush.exe", "rush.com"]
        } else {
            &["rush"]
        };
        let path = std::env::var_os("PATH");
        let extensions = if cfg!(windows) {
            None
        } else {
            std::env::var_os("PATHEXT")
        };
        let program = names
            .iter()
            .find_map(|name| tree::resolve(name, path.as_deref(), extensions.as_deref()))?;
        // CreateProcessW cannot run a `.cmd`/`.bat` without a shell; only a spawnable program
        // is accepted (fail closed).
        if !crate::ai::provider::spawnable_program(&program) {
            return None;
        }
        Some(Self::new(
            RushCommand {
                program: program.to_string_lossy().into_owned(),
                prefix: Vec::new(),
            },
            std::env::temp_dir().join("alef-rush"),
        ))
    }

    /// Builds an explicit command.
    pub fn new(command: RushCommand, data_root: PathBuf) -> Self {
        Self {
            command,
            data_root: Arc::new(ProviderRoot::new(data_root)),
            chat_deadline: limits::CHAT_TIME,
            envelope: false,
            sessions: Arc::new(Mutex::new(SessionMap::default())),
            roles_cache: Mutex::new(None),
        }
    }

    pub async fn data_dir(&self) -> Result<PathBuf, AlefError> {
        self.data_root.get().await
    }
    /// Test seam.
    pub fn with_envelope(mut self, envelope: bool) -> Self {
        self.envelope = envelope;
        self
    }

    /// Test-only deadline override.
    pub fn with_chat_deadline(mut self, deadline: Duration) -> Self {
        self.chat_deadline = deadline;
        self
    }

    /// Serves available roles.
    fn roles_of(&self, outcome: ProbeOutcome) -> Result<Vec<Role>, AlefError> {
        let roles = match outcome {
            ProbeOutcome::Json(value) => served_slots(&value),
            ProbeOutcome::TimedOut => {
                return Err(AlefError::new(
                    ErrorCode::Timeout,
                    "listing the roles took too long",
                ));
            }
            ProbeOutcome::Failed => {
                return Err(AlefError::new(
                    ErrorCode::Internal,
                    "the rush models probe failed",
                ));
            }
        }
        .into_iter()
        .map(|slot| Role {
            id: slot.to_owned(),
            title: if slot == "smart" { "Smart" } else { "Fast" }.to_owned(),
            description: "served by the user's rush command-line".to_owned(),
            capabilities: vec!["text".to_owned(), "usage".to_owned(), "cancel".to_owned()],
        })
        .collect::<Vec<_>>();
        *self
            .roles_cache
            .lock()
            .unwrap_or_else(|error| error.into_inner()) =
            Some((std::time::Instant::now(), roles.clone()));
        Ok(roles)
    }

    fn cached_roles(&self) -> Option<Vec<Role>> {
        self.roles_cache
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .as_ref()
            .filter(|(at, _)| at.elapsed() < ROLES_CACHE_TTL)
            .map(|(_, roles)| roles.clone())
    }
}

impl super::provider::AiProvider for RushCliProvider {
    /// Sync provider form.
    fn roles(&self) -> Result<Vec<Role>, AlefError> {
        self.cached_roles()
            .ok_or_else(|| AlefError::new(ErrorCode::NotAvailable, "roles require an async probe"))
    }

    /// Probe cache time-to-live.
    fn roles_fut(&self) -> Pin<Box<dyn Future<Output = Result<Vec<Role>, AlefError>> + Send + '_>> {
        Box::pin(async {
            if let Some(roles) = self.cached_roles() {
                return Ok(roles);
            }
            let command = self.command.clone();
            let root = self.data_root.get().await?;
            let outcome = run_probe(&command, &root).await;
            self.roles_of(outcome)
        })
    }

    fn token_bound(&self, _request: &ChatRequest) -> Option<u64> {
        Some(MAX_TOKENS)
    }

    fn chat(
        &self,
        request: ChatRequest,
        control: ChatControl,
    ) -> Result<mpsc::Receiver<ChatEvent>, AlefError> {
        // R1: rush cannot take application or builtin tools yet.
        if !request.tools.is_empty() || !request.builtin.is_empty() {
            return Err(AlefError::new(
                ErrorCode::NotAvailable,
                "rush cannot take application tools yet",
            ));
        }
        if request.role != "smart" && request.role != "fast" {
            return Err(AlefError::new(
                ErrorCode::NotAvailable,
                "the role is not available",
            ));
        }
        let (sender, receiver) = mpsc::channel(4);
        let task = ChatTask {
            command: self.command.clone(),
            data_root: self.data_root.clone(),
            chat_deadline: self.chat_deadline,
            envelope: self.envelope,
            sessions: self.sessions.clone(),
            request,
        };
        tokio::spawn(chat::run_chat(task, control, sender));
        Ok(receiver)
    }

    /// Clears stored conversation state.
    fn forget(&self, request: &ChatRequest) {
        let Some(conversation) = &request.conversation else {
            return;
        };
        let id = self
            .sessions
            .lock()
            .unwrap_or_else(|error| error.into_inner())
            .remove(conversation);
        let Some(id) = id else { return };
        let root = self.data_root.clone();
        let command = self.command.clone();
        tokio::spawn(delete_sessions_owned(root, command, vec![id]));
    }
}

pub(crate) async fn delete_sessions_owned(
    root: Arc<ProviderRoot>,
    command: RushCommand,
    ids: Vec<String>,
) {
    if let Ok(path) = root.get().await {
        for id in ids {
            let args = vec![
                "sessions".into(),
                "delete".into(),
                id,
                "--data-dir".into(),
                path.to_string_lossy().into_owned(),
            ];
            let cmd = command.clone();
            let workroot = path.clone();
            if let Ok(job) = builtin::executor().submit(move || sync::run(&cmd, &workroot, &args)) {
                let _ = job.result().await;
            }
        }
    }
}

impl Drop for RushCliProvider {
    fn drop(&mut self) {
        let root = self.data_root.clone();
        let command = self.command.clone();
        let sessions = self.sessions.clone();
        if let Ok(handle) = tokio::runtime::Handle::try_current() {
            handle.spawn(async move {
                let ids = sessions
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .ids(usize::MAX);
                delete_sessions_owned(root, command, ids).await;
            });
        }
    }
}

impl std::fmt::Debug for RushCliProvider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("RushCliProvider")
            .field("command", &self.command)
            .field("data_root", &self.data_root.path.get())
            .field("chat_deadline", &self.chat_deadline)
            .field("envelope", &self.envelope)
            .finish_non_exhaustive()
    }
}
