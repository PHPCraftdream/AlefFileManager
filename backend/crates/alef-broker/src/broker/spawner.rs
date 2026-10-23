// SPDX-License-Identifier: MIT OR Apache-2.0
//! Spare-process spawning ([`Spawner`]) and the process abstraction used by
//! the broker's bookkeeping ([`SpareProcess`]).
//!
//! The real spawner runs the SAME executable in the `warm-spare` role:
//! `exe <extra_args...> warm-spare --kind <windowed|windowless>
//! --spare-id <n> <endpoint>` where `<endpoint>` is the broker entry point
//! the spare connects to (Unix socket path / Windows pipe name).
//!
//! The challenge token is NEVER passed in argv or the environment: the spare
//! reads the token file itself (`ipc::token`), which lives NEXT TO the
//! endpoint — Unix: `<endpoint.unix_dir>/broker.token` (0600 in the 0700
//! broker directory); Windows:
//! `%LOCALAPPDATA%\alef\broker\<endpoint.name>\broker.token`. Same-user
//! inheritance of the directory ACLs authenticates the spare like any other
//! local client.
//!
//! stdout/stderr are piped (the broker relays them as `WarmRelayMsg::Output`);
//! stdin is null; the process gets no inherited console (Windows
//! `CREATE_NO_WINDOW`) and starts its own process group (Unix `process_group(0)`,
//! stable since Rust 1.64; the crate pins 1.88).

use std::ffi::OsString;
use std::io;
use std::path::PathBuf;
use std::process::{Child, Command, Stdio};

use crate::ipc::endpoint::Endpoint;
use crate::pool::SlotKind;
use crate::protocol::warm;

/// Reuse the pool's spare identifier: the broker, the pool and the spawned
/// argv all speak about the same id.
pub use crate::pool::SpareId;

/// How a spare process ended (docs/stages/m11-warm-start.md, «Протокол»):
/// Unix rule — death by signal `n` is reported as `code = 128 + n` with
/// `signal: Some(n)`; a normal exit has `signal: None`. On Windows there are
/// no signals: `code` is the exit code as reported by the OS.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ExitInfo {
    pub code: i32,
    pub signal: Option<i32>,
}

/// A live spare process the broker owns. `Send` so ownership can move to the
/// broker's relay/monitor threads.
pub trait SpareProcess: Send {
    fn pid(&self) -> u32;
    /// Terminate the process; errors (already dead) are ignored by callers.
    fn kill(&self);
    /// Non-blocking exit poll; `Ok(None)` = still running.
    fn try_wait(&mut self) -> io::Result<Option<ExitInfo>>;
    /// Take the stdout pipe; `None` after the first take.
    fn stdout(&mut self) -> Option<Box<dyn io::Read + Send>>;
    /// Take the stderr pipe; `None` after the first take.
    fn stderr(&mut self) -> Option<Box<dyn io::Read + Send>>;
}

/// Creates spare processes. `Send + Sync` so the broker can share one spawner
/// with its threads.
pub trait Spawner: Send + Sync {
    fn spawn(&self, kind: SlotKind, id: SpareId) -> io::Result<Box<dyn SpareProcess>>;
}

/// The real spawner: launches this runtime's executable in the `warm-spare`
/// role against the broker's endpoint.
pub struct ProcessSpawner {
    exe: PathBuf,
    extra_args: Vec<OsString>,
    endpoint: Endpoint,
}

impl ProcessSpawner {
    pub fn new(exe: PathBuf, extra_args: Vec<OsString>, endpoint: Endpoint) -> ProcessSpawner {
        ProcessSpawner {
            exe,
            extra_args,
            endpoint,
        }
    }

    /// The endpoint argument appended to the spare's argv: the broker entry
    /// point the spare dials (Unix socket path / Windows pipe name).
    pub fn endpoint_arg(&self) -> OsString {
        #[cfg(unix)]
        {
            self.endpoint.unix_socket.clone().into_os_string()
        }
        #[cfg(windows)]
        {
            OsString::from(self.endpoint.windows_pipe.clone())
        }
    }

    fn kind_arg(kind: SlotKind) -> &'static str {
        match kind {
            SlotKind::Windowed => "windowed",
            SlotKind::Windowless => "windowless",
        }
    }
}

impl Spawner for ProcessSpawner {
    fn spawn(&self, kind: SlotKind, id: SpareId) -> io::Result<Box<dyn SpareProcess>> {
        let mut cmd = Command::new(&self.exe);
        cmd.args(&self.extra_args)
            .arg("warm-spare")
            .arg("--kind")
            .arg(Self::kind_arg(kind))
            .arg("--spare-id")
            .arg(id.0.to_string())
            .arg(&self.endpoint_arg())
            .stdin(Stdio::null())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());

        // No inherited console; own process group so the broker can signal
        // the spare without dragging the broker's own group along.
        #[cfg(unix)]
        {
            use std::os::unix::process::CommandExt;
            cmd.process_group(0); // stable since 1.64; crate pins rust-version 1.88
        }
        #[cfg(windows)]
        {
            use std::os::windows::process::CommandExt;
            const CREATE_NO_WINDOW: u32 = 0x0800_0000;
            cmd.creation_flags(CREATE_NO_WINDOW);
        }

        let child = cmd.spawn()?;
        Ok(Box::new(StdSpareProcess { child }))
    }
}

/// [`SpareProcess`] over a real `std::process::Child`.
struct StdSpareProcess {
    child: Child,
}

impl SpareProcess for StdSpareProcess {
    fn pid(&self) -> u32 {
        self.child.id()
    }

    fn kill(&self) {
        let _ = self.child.kill();
    }

    fn try_wait(&mut self) -> io::Result<Option<ExitInfo>> {
        Ok(self.child.try_wait()?.map(|status| exit_info(&status)))
    }

    fn stdout(&mut self) -> Option<Box<dyn io::Read + Send>> {
        self.child.stdout.take().map(|s| Box::new(s) as _)
    }

    fn stderr(&mut self) -> Option<Box<dyn io::Read + Send>> {
        self.child.stderr.take().map(|s| Box::new(s) as _)
    }
}

fn exit_info(status: &std::process::ExitStatus) -> ExitInfo {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(sig) = status.signal() {
            ExitInfo {
                code: 128 + sig,
                signal: Some(sig),
            }
        } else {
            ExitInfo {
                code: status.code().unwrap_or(-1),
                signal: None,
            }
        }
    }
    #[cfg(windows)]
    {
        ExitInfo {
            code: status.code().unwrap_or(-1),
            signal: None,
        }
    }
}

/// Wire kind is never constructed here yet, but keep the mapping total:
/// `warm::SlotKind` (wire) ↔ `pool::SlotKind` (internal) stay in sync.
#[allow(dead_code)]
pub(crate) fn wire_kind(kind: SlotKind) -> warm::SlotKind {
    match kind {
        SlotKind::Windowed => warm::SlotKind::Windowed,
        SlotKind::Windowless => warm::SlotKind::Windowless,
    }
}

#[allow(dead_code)]
pub(crate) fn from_wire_kind(kind: warm::SlotKind) -> SlotKind {
    match kind {
        warm::SlotKind::Windowed => SlotKind::Windowed,
        warm::SlotKind::Windowless => SlotKind::Windowless,
    }
}
