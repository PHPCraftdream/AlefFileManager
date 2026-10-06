// SPDX-License-Identifier: MIT OR Apache-2.0
//! Runtime authorization: the manifest's permission policy plus per-session grants.
use super::{
    manifest::Permissions,
    scope::{
        canonical,
        exec::ExecScope,
        invalid,
        net::{SocketScope, UrlScope},
        path::{parts, PathPattern},
    },
};
use crate::{AlefError, ErrorCode};
use serde_json::json;
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
};

pub use super::grants::Grants;

/// A capability a command may require.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum Permission {
    /// No capability needed.
    None,
    /// Read files inside `fs.read` scopes or runtime grants.
    FsRead,
    /// Write files inside `fs.write` scopes or runtime grants.
    FsWrite,
    /// Run a program listed in `cli.exec`.
    CliExec,
    /// HTTP(S)/WebSocket request to a `net.http` target.
    NetHttp,
    /// Raw socket to a `net.socket` target.
    NetSocket,
    /// Open a `shell.openExternal` URL.
    ShellOpenExternal,
    /// Read the clipboard (`clipboard.read`).
    ClipboardRead,
    /// Register global shortcuts (`shortcut.global`).
    ShortcutGlobal,
    /// Use the secret store (`secrets`).
    Secrets,
    /// Read an environment variable listed in `app.env`.
    AppEnv,
    /// Create windows at runtime (`permissions.window.create` in the manifest, or the embedder).
    WindowCreate,
}

impl Permission {
    /// Manifest-style permission name used in error details.
    pub fn name(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::FsRead => "fs.read",
            Self::FsWrite => "fs.write",
            Self::CliExec => "cli.exec",
            Self::NetHttp => "net.http",
            Self::NetSocket => "net.socket",
            Self::ShellOpenExternal => "shell.openExternal",
            Self::ClipboardRead => "clipboard.read",
            Self::ShortcutGlobal => "shortcut.global",
            Self::Secrets => "secrets",
            Self::AppEnv => "app.env",
            Self::WindowCreate => "window.create",
        }
    }
}

/// Runtime path variables available to manifest scope patterns.
#[derive(Debug, Clone)]
pub struct PathVars {
    /// App data root (`$APPDATA`).
    pub app_data: PathBuf,
    /// App config root (`$APPCONFIG`).
    pub app_config: PathBuf,
    /// App cache root (`$APPCACHE`).
    pub app_cache: PathBuf,
    /// Home directory (`$HOME`).
    pub home: PathBuf,
    /// Documents directory (`$DOCUMENTS`).
    pub documents: PathBuf,
    /// Downloads directory (`$DOWNLOADS`).
    pub downloads: PathBuf,
    /// Desktop directory (`$DESKTOP`).
    pub desktop: PathBuf,
    /// Temporary directory (`$TEMP`).
    pub temp: PathBuf,
    /// Application directory (`$APP`).
    pub app: PathBuf,
}

impl PathVars {
    fn lookup(&self, name: &str) -> Option<&Path> {
        Some(match name {
            "APPDATA" => &self.app_data,
            "APPCONFIG" => &self.app_config,
            "APPCACHE" => &self.app_cache,
            "HOME" => &self.home,
            "DOCUMENTS" => &self.documents,
            "DOWNLOADS" => &self.downloads,
            "DESKTOP" => &self.desktop,
            "TEMP" => &self.temp,
            "APP" => &self.app,
            _ => return None,
        })
    }
}

/// Validated permission policy; every scope is parsed once at load and matching fails closed.
#[derive(Debug, Clone)]
pub struct PermissionSet {
    read: Vec<PathPattern>,
    write: Vec<PathPattern>,
    exec: Vec<ExecScope>,
    http: Vec<UrlScope>,
    socket: Vec<SocketScope>,
    shell: Vec<UrlScope>,
    env: HashSet<String>,
    clipboard: bool,
    shortcut: bool,
    secrets: bool,
    window: bool,
}

/// `$VAR` is only valid as the first path segment; its value is taken literally.
fn path_pattern(pattern: &str, vars: &PathVars) -> Result<PathPattern, AlefError> {
    match pattern.strip_prefix('$') {
        Some(rest) => {
            let end = rest.find(['/', '\\']).unwrap_or(rest.len());
            let root = vars
                .lookup(&rest[..end])
                .ok_or_else(|| invalid(format!("unknown path variable ${}", &rest[..end])))?;
            PathPattern::parse(Some(root), rest[end..].trim_start_matches(['/', '\\']))
        }
        None if pattern.contains('$') => Err(invalid("path variable must be the first segment")),
        None => PathPattern::parse(None, pattern),
    }
}

fn denied(permission: Permission) -> AlefError {
    AlefError::new(ErrorCode::PermissionDenied, "permission denied")
        .with_details(json!({"permission": permission.name()}))
}

impl PermissionSet {
    /// Expands variables and validates every scope; a malformed scope is `MANIFEST_INVALID`.
    pub fn from_manifest(policy: &Permissions, vars: &PathVars) -> Result<Self, AlefError> {
        let paths = |list: &[String]| -> Result<Vec<PathPattern>, AlefError> {
            list.iter().map(|p| path_pattern(p, vars)).collect()
        };
        let urls = |list: &[String]| -> Result<Vec<UrlScope>, AlefError> {
            list.iter().map(|p| UrlScope::parse(p)).collect()
        };
        let env = policy
            .app
            .env
            .iter()
            .map(|name| {
                if super::scope::clean(name) && !name.contains('=') {
                    Ok(name.clone())
                } else {
                    Err(invalid("invalid environment variable name"))
                }
            })
            .collect::<Result<_, _>>()?;
        Ok(Self {
            read: paths(&policy.fs.read)?,
            write: paths(&policy.fs.write)?,
            exec: policy
                .cli
                .exec
                .iter()
                .map(|e| ExecScope::parse(e))
                .collect::<Result<_, _>>()?,
            http: urls(&policy.net.http)?,
            socket: policy
                .net
                .socket
                .iter()
                .map(|s| SocketScope::parse(s))
                .collect::<Result<_, _>>()?,
            shell: urls(&policy.shell.open_external)?,
            env,
            clipboard: policy.clipboard.read,
            shortcut: policy.shortcut.global,
            secrets: policy.secrets,
            window: policy.window.as_ref().is_some_and(|window| window.create),
        })
    }

    /// Names of the environment variables the manifest exposes (`permissions.app.env`), sorted.
    pub fn env_names(&self) -> Vec<&str> {
        let mut names: Vec<&str> = self.env.iter().map(String::as_str).collect();
        names.sort_unstable();
        names
    }

    /// Allows runtime window creation regardless of the manifest (for an embedding host).
    pub fn with_window_create(mut self) -> Self {
        self.window = true;
        self
    }

    /// Checks `permission` for `target`; scoped permissions deny a missing target, and every
    /// failure is a uniform `PERMISSION_DENIED` naming only the permission.
    pub fn check(
        &self,
        permission: Permission,
        target: Option<&str>,
        grants: &Grants,
    ) -> Result<(), AlefError> {
        let any = |matched: &dyn Fn(&str) -> bool| target.is_some_and(matched);
        let allowed = match permission {
            Permission::None => true,
            Permission::ClipboardRead => self.clipboard,
            Permission::ShortcutGlobal => self.shortcut,
            Permission::Secrets => self.secrets,
            Permission::WindowCreate => self.window,
            Permission::AppEnv => target.is_some_and(|name| self.env.contains(name)),
            Permission::FsRead | Permission::FsWrite => {
                return self.authorize_path(permission, target, grants).map(|_| ());
            }
            Permission::CliExec => any(&|t| self.exec.iter().any(|s| s.matches(t))),
            Permission::NetHttp => any(&|t| self.http.iter().any(|s| s.matches(t))),
            Permission::NetSocket => any(&|t| self.socket.iter().any(|s| s.matches(t))),
            Permission::ShellOpenExternal => any(&|t| self.shell.iter().any(|s| s.matches(t))),
        };
        if allowed {
            Ok(())
        } else {
            Err(denied(permission))
        }
    }

    /// Authorizes a filesystem target and returns its canonical path. Callers must operate on the
    /// returned path, not on the original string, so that checked and used paths are the same.
    pub fn authorize_path(
        &self,
        permission: Permission,
        target: Option<&str>,
        grants: &Grants,
    ) -> Result<PathBuf, AlefError> {
        let write = match permission {
            Permission::FsRead => false,
            Permission::FsWrite => true,
            _ => {
                return Err(AlefError::new(
                    ErrorCode::InvalidArgument,
                    "not a filesystem permission",
                ))
            }
        };
        let resolved = target
            .and_then(|t| canonical(Path::new(t)))
            .ok_or_else(|| denied(permission))?;
        let components = parts(&resolved);
        let scopes = if write { &self.write } else { &self.read };
        if scopes.iter().any(|s| s.matches_canonical(&components))
            || grants.allows(write, &components)
        {
            Ok(resolved)
        } else {
            Err(denied(permission))
        }
    }
}
