// SPDX-License-Identifier: MIT OR Apache-2.0
//! Framework modules: desktop (`app`, `window`, `dialog`, `shell`), system (`path`, `os`, `screen`,
//! `clipboard`); more arrive with M2 and later.
//!
//! A module registers its commands in the registry (`<module>.<command>`) with the permission and
//! scope each one needs; the registry checks them before the handler runs. Everything the modules
//! need from the process comes through [`ModuleContext`] and the [`Host`](alef_core::registry::host::Host).
use std::{ffi::OsString, path::PathBuf, sync::Arc};

use alef_core::{
    registry::{command::Reply, dispatch::Registry, host::Host},
    security::permissions::PathVars,
    AlefError, ErrorCode,
};
use serde::{Deserialize, Serialize};

pub mod desktop;
pub mod system;

pub use desktop::{
    args::{ArgValue, ParsedArgs},
    shell::{PretendShell, ShellBackend, ShellRequest, SystemShell},
};
pub use system::clipboard::{ClipboardBackend, Image, MemoryClipboard, SystemClipboard};

/// What the clipboard and the desktop shell really do. The system ones act on the desktop of the
/// user; the pretending ones answer as if they did and leave it alone.
#[derive(Debug, Clone)]
pub struct Backends {
    pub clipboard: Arc<dyn ClipboardBackend>,
    pub shell: Arc<dyn ShellBackend>,
}

impl Backends {
    /// The clipboard and the shell of the desktop.
    pub fn system() -> Self {
        Self {
            clipboard: Arc::new(SystemClipboard::default()),
            shell: Arc::new(SystemShell),
        }
    }

    /// A clipboard in memory and a shell that does nothing and, given a file, logs its requests.
    pub fn pretending(shell_log: Option<PathBuf>) -> Self {
        Self {
            clipboard: Arc::new(MemoryClipboard::default()),
            shell: Arc::new(match shell_log {
                Some(path) => PretendShell::logging_to(path),
                None => PretendShell::default(),
            }),
        }
    }

    /// An end-to-end run (`ALEF_E2E=1`) pretends, its requests of the shell go to the file
    /// `ALEF_E2E_SHELL_LOG` names; every other run uses the desktop.
    pub fn from_environment() -> Self {
        let flag = |name: &str| std::env::var(name).is_ok_and(|value| value == "1");
        if flag("ALEF_E2E") {
            Self::pretending(std::env::var_os("ALEF_E2E_SHELL_LOG").map(PathBuf::from))
        } else {
            Self::system()
        }
    }
}

/// Identity of the running application (`app.info`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "modules.ts")]
pub struct AppInfo {
    pub id: String,
    pub name: String,
    pub version: String,
    /// Version of the Alef runtime executing the application.
    pub runtime_version: String,
}

/// The facts about the process that the modules serve.
#[derive(Debug, Clone)]
pub struct ModuleContext {
    pub app: AppInfo,
    pub paths: PathVars,
    /// The command line of the application, already parsed by its schema.
    pub args: ParsedArgs,
    /// Arguments of this process after the program name; `app.relaunch` starts it again with them.
    pub process_args: Vec<OsString>,
    /// The clipboard and the shell the modules act through.
    pub backends: Backends,
}

/// Registers every module of this crate.
pub fn register_all(
    registry: &mut Registry,
    host: Arc<dyn Host>,
    context: &ModuleContext,
) -> Result<(), AlefError> {
    desktop::register(registry, host.clone(), context)?;
    system::register(registry, host, context)
}

pub(crate) fn json<T: Serialize>(value: &T) -> Result<Reply, AlefError> {
    serde_json::to_value(value)
        .map(Reply::Json)
        .map_err(|error| AlefError::new(ErrorCode::Internal, error.to_string()))
}
