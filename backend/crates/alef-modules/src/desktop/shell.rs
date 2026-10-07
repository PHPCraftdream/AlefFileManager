// SPDX-License-Identifier: MIT OR Apache-2.0
//! `shell`: hands a URL or a file to the desktop. `openExternal` needs the URL inside the
//! `shell.openExternal` scope; `openPath` and `showInFolder` read access to the path, `trash` write
//! access. `openPath` never starts a program: what the desktop would run instead of show (an
//! executable, a script, a shortcut, an application bundle) is refused, otherwise "read and open"
//! would be a way around `cli.exec`.
#[cfg(any(windows, target_os = "macos"))]
use std::process::{Command, Stdio};
use std::{
    fmt::Debug,
    fs,
    io::{ErrorKind, Write},
    path::{Path, PathBuf},
    sync::{Arc, Mutex},
};

use alef_core::{
    registry::{command::Reply, context::CallContext, dispatch::Registry},
    security::{consent::Decision, permissions::Permission},
    AlefError, ErrorCode,
};
use serde::Deserialize;

/// What the desktop is asked to do. Calls may block; the module runs them off the async threads.
pub trait ShellBackend: Send + Sync + Debug {
    /// Opens a web address in the browser of the user.
    fn open_external(&self, url: &str) -> Result<(), AlefError>;
    /// Opens a file or folder with the program the desktop has for it.
    fn open_path(&self, path: &Path) -> Result<(), AlefError>;
    /// Shows the file in the file manager, selected where the file manager can do that.
    fn show_in_folder(&self, path: &Path) -> Result<(), AlefError>;
    /// Moves the file or folder to the trash.
    fn trash(&self, path: &Path) -> Result<(), AlefError>;
}

/// A request a [`PretendShell`] received.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ShellRequest {
    pub operation: &'static str,
    pub target: String,
}

/// A desktop that says yes and does nothing: for runs that must not open windows, browsers or
/// file managers or delete files of the user. Keeps what it was asked and, when given a file,
/// appends it there as one JSON line per request.
#[derive(Debug, Default)]
pub struct PretendShell {
    log: Option<PathBuf>,
    asked: Mutex<Vec<ShellRequest>>,
}

impl PretendShell {
    pub fn logging_to(path: PathBuf) -> Self {
        Self {
            log: Some(path),
            asked: Mutex::default(),
        }
    }

    /// What it was asked, in order.
    pub fn asked(&self) -> Vec<ShellRequest> {
        self.asked.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    fn take(&self, operation: &'static str, target: &str) -> Result<(), AlefError> {
        let mut asked = self.asked.lock().unwrap_or_else(|e| e.into_inner());
        asked.push(ShellRequest {
            operation,
            target: target.to_owned(),
        });
        if let Some(path) = &self.log {
            let line = serde_json::json!({ "operation": operation, "target": target });
            fs::OpenOptions::new()
                .create(true)
                .append(true)
                .open(path)
                .and_then(|mut file| writeln!(file, "{line}"))
                .map_err(|error| {
                    AlefError::new(ErrorCode::Internal, format!("shell log: {error}"))
                })?;
        }
        Ok(())
    }
}

impl ShellBackend for PretendShell {
    fn open_external(&self, url: &str) -> Result<(), AlefError> {
        self.take("openExternal", url)
    }
    fn open_path(&self, path: &Path) -> Result<(), AlefError> {
        self.take("openPath", &path.to_string_lossy())
    }
    fn show_in_folder(&self, path: &Path) -> Result<(), AlefError> {
        self.take("showInFolder", &path.to_string_lossy())
    }
    fn trash(&self, path: &Path) -> Result<(), AlefError> {
        self.take("trash", &path.to_string_lossy())
    }
}

/// The desktop of the user.
#[derive(Debug, Default)]
pub struct SystemShell;

fn failed(what: &str, error: impl std::fmt::Display) -> AlefError {
    AlefError::new(ErrorCode::Internal, format!("shell.{what}: {error}"))
}

impl ShellBackend for SystemShell {
    fn open_external(&self, url: &str) -> Result<(), AlefError> {
        opener::open_browser(url).map_err(|error| failed("openExternal", error))
    }

    fn open_path(&self, path: &Path) -> Result<(), AlefError> {
        opener::open(path).map_err(|error| failed("openPath", error))
    }

    fn show_in_folder(&self, path: &Path) -> Result<(), AlefError> {
        reveal(path).map_err(|error| failed("showInFolder", error))
    }

    fn trash(&self, path: &Path) -> Result<(), AlefError> {
        trash::delete(path).map_err(|error| failed("trash", error))
    }
}

/// Starts the file manager on the file and returns without waiting for it.
#[cfg(windows)]
fn reveal(path: &Path) -> std::io::Result<()> {
    use std::os::windows::process::CommandExt;
    // Explorer reads the whole tail as one argument; a path never holds a quote on Windows.
    Command::new("explorer.exe")
        .raw_arg(format!("/select,\"{}\"", path.display()))
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(drop)
}

#[cfg(target_os = "macos")]
fn reveal(path: &Path) -> std::io::Result<()> {
    Command::new("open")
        .arg("-R")
        .arg(path)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .map(drop)
}

/// There is no common way to select a file: the folder that holds it is opened.
#[cfg(not(any(windows, target_os = "macos")))]
fn reveal(path: &Path) -> std::io::Result<()> {
    let folder = if path.is_dir() {
        Some(path)
    } else {
        path.parent()
    };
    let folder = folder.ok_or_else(|| std::io::Error::from(ErrorKind::NotFound))?;
    opener::open(folder).map_err(std::io::Error::other)
}

const WINDOWS_PROGRAMS: &[&str] = &[
    "exe",
    "com",
    "bat",
    "cmd",
    "ps1",
    "psm1",
    "vbs",
    "vbe",
    "js",
    "jse",
    "wsf",
    "wsh",
    "msi",
    "msp",
    "scr",
    "lnk",
    "url",
    "hta",
    "cpl",
    "reg",
    "pif",
    "jar",
    "msc",
    "application",
    "gadget",
    "appref-ms",
    "mht",
    "settingcontent-ms",
];

const MAC_PROGRAMS: &[&str] = &[
    "app", "command", "sh", "tool", "pkg", "mpkg", "scpt", "workflow", "terminal", "jar",
];

/// Whether opening the path would run something instead of showing it. Judged on the name on
/// every system (a file renamed to `.bat` and copied to Windows is still a script) and, where
/// there are execute bits, on those too.
pub(crate) fn opens_by_running(path: &Path) -> bool {
    let extension = path
        .extension()
        .map(|e| e.to_string_lossy().to_ascii_lowercase())
        .unwrap_or_default();
    if WINDOWS_PROGRAMS.contains(&extension.as_str())
        || MAC_PROGRAMS.contains(&extension.as_str())
        || extension == "desktop"
    {
        return true;
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        if let Ok(meta) = fs::metadata(path) {
            return meta.is_file() && meta.permissions().mode() & 0o111 != 0;
        }
    }
    false
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UrlArgs {
    url: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct PathArgs {
    path: String,
}

async fn blocking(
    backend: &Arc<dyn ShellBackend>,
    work: impl FnOnce(&dyn ShellBackend) -> Result<(), AlefError> + Send + 'static,
) -> Result<(), AlefError> {
    let backend = backend.clone();
    tokio::task::spawn_blocking(move || work(backend.as_ref()))
        .await
        .map_err(|error| AlefError::new(ErrorCode::Internal, format!("shell: {error}")))?
}

fn missing(path: &Path) -> AlefError {
    AlefError::new(
        ErrorCode::NotFound,
        format!("shell: {} does not exist", path.display()),
    )
}

/// The path must be there (a dangling link counts as there); the answer is the same whichever
/// desktop is behind the module.
fn existing(path: &Path) -> Result<(), AlefError> {
    match fs::symlink_metadata(path) {
        Ok(_) => Ok(()),
        Err(error) if error.kind() == ErrorKind::NotFound => Err(missing(path)),
        Err(error) => Err(failed("path", error)),
    }
}

/// The desktop of the user and the stand-in for a command whose right was given as a stand-in.
struct Pair {
    real: Arc<dyn ShellBackend>,
    stand_in: Arc<dyn ShellBackend>,
}

impl Pair {
    /// What the user decided for the right of this command (the address, the path) picks the desktop.
    fn pick(&self, ctx: &CallContext) -> Arc<dyn ShellBackend> {
        if ctx.decision() == Decision::Substitute {
            self.stand_in.clone()
        } else {
            self.real.clone()
        }
    }
}

pub(crate) fn register(
    registry: &mut Registry,
    backend: Arc<dyn ShellBackend>,
) -> Result<(), AlefError> {
    let pair = Arc::new(Pair {
        real: backend,
        stand_in: Arc::new(PretendShell::default()),
    });
    let this = pair.clone();
    registry
        .command::<UrlArgs>("shell.openExternal")?
        .permission(Permission::ShellOpenExternal, |args| Some(args.url.clone()))
        .substitutes()
        .handler(move |ctx, args| {
            let backend = this.pick(&ctx);
            async move {
                blocking(&backend, move |b| b.open_external(&args.url)).await?;
                Ok(Reply::Json(serde_json::Value::Null))
            }
        })?;
    let this = pair.clone();
    registry
        .command::<PathArgs>("shell.openPath")?
        .permission(Permission::FsRead, |args| Some(args.path.clone()))
        .substitutes()
        .handler(move |ctx, args| {
            let backend = this.pick(&ctx);
            async move {
                let path = ctx.permissions.authorize_path(
                    Permission::FsRead,
                    Some(&args.path),
                    &ctx.grants(),
                )?;
                existing(&path)?;
                if opens_by_running(&path) {
                    return Err(AlefError::new(
                        ErrorCode::PermissionDenied,
                        "shell.openPath does not start programs",
                    ));
                }
                blocking(&backend, move |b| b.open_path(&path)).await?;
                Ok(Reply::Json(serde_json::Value::Null))
            }
        })?;
    let this = pair.clone();
    registry
        .command::<PathArgs>("shell.showInFolder")?
        .permission(Permission::FsRead, |args| Some(args.path.clone()))
        .substitutes()
        .handler(move |ctx, args| {
            let backend = this.pick(&ctx);
            async move {
                let path = ctx.permissions.authorize_path(
                    Permission::FsRead,
                    Some(&args.path),
                    &ctx.grants(),
                )?;
                existing(&path)?;
                blocking(&backend, move |b| b.show_in_folder(&path)).await?;
                Ok(Reply::Json(serde_json::Value::Null))
            }
        })?;
    registry
        .command::<PathArgs>("shell.trash")?
        .permission(Permission::FsWrite, |args| Some(args.path.clone()))
        .substitutes()
        .handler(move |ctx, args| {
            let backend = pair.pick(&ctx);
            async move {
                let path = ctx.permissions.authorize_path(
                    Permission::FsWrite,
                    Some(&args.path),
                    &ctx.grants(),
                )?;
                existing(&path)?;
                blocking(&backend, move |b| b.trash(&path)).await?;
                Ok(Reply::Json(serde_json::Value::Null))
            }
        })
}
