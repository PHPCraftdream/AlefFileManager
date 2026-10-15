// SPDX-License-Identifier: MIT OR Apache-2.0
//! Runtime authorization: the manifest's permission policy, the decisions of the user and the
//! per-session grants.
use super::{
    consent::{Consent, Decision, Right},
    manifest::Permissions,
    scope::{
        canonical, canonical_entry,
        command::{parse_cli, DeclaredCommand},
        exec::ExecScope,
        invalid,
        net::{SocketScope, UrlScope},
        path::{parts, same, PathPattern},
    },
};
use crate::{AlefError, ErrorCode};
use serde_json::json;
use std::{
    collections::HashSet,
    path::{Path, PathBuf},
    sync::{Arc, RwLock, RwLockReadGuard},
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
    /// Run a declared command (`cli.commands`); the target is its name.
    CliCommand,
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
    /// Manage a tray icon (`tray`).
    Tray,
    /// Use the secret store (`secrets`).
    Secrets,
    /// Invoke items of the system context menu (`shell.contextMenu`).
    ShellContextMenu,
    /// Use the WebCrypto of the engine (`webcrypto`).
    WebCrypto,
    /// Read an environment variable listed in `app.env`.
    AppEnv,
    /// Manage application autostart (`app.autostart`).
    AppAutostart,
    /// Manage declared deep-link schemes (`app.deepLinks`); the target is a scheme.
    AppDeepLinks,
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
            Self::CliCommand => "cli.command",
            Self::NetHttp => "net.http",
            Self::NetSocket => "net.socket",
            Self::ShellOpenExternal => "shell.openExternal",
            Self::ClipboardRead => "clipboard.read",
            Self::ShortcutGlobal => "shortcut.global",
            Self::Tray => "tray",
            Self::Secrets => "secrets",
            Self::ShellContextMenu => "shell.contextMenu",
            Self::WebCrypto => "webcrypto",
            Self::AppEnv => "app.env",
            Self::AppDeepLinks => "app.deepLinks",
            Self::AppAutostart => "app.autostart",
            Self::WindowCreate => "window.create",
        }
    }
}

/// Whether the last component of a path is followed when it is a link.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Reach {
    /// The path means what it leads to (reading a file, listing a folder).
    Through,
    /// The path means the entry itself (removing or renaming a link, `lstat`).
    Entry,
}

/// Where a stand-in keeps what the application believes to be at a path: the scope the path lies in
/// (as the manifest wrote it) and the place inside that scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Shadow {
    pub scope: String,
    pub inside: PathBuf,
}

/// A filesystem path the user's decisions let the application use.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Authorized {
    /// The canonical path as the application knows it.
    pub path: PathBuf,
    pub decision: Decision,
    /// Set when the decision is `Substitute`: where the stand-in keeps the path.
    pub shadow: Option<Shadow>,
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
    pub app: Option<PathBuf>,
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
            "APP" => return self.app.as_deref(),
            _ => return None,
        })
    }
}

/// One entry of a scope list of the manifest: the text the manifest wrote (the user decides on
/// that text) and what it was parsed to.
#[derive(Debug, Clone)]
struct Scoped<T> {
    raw: String,
    scope: T,
}

/// Validated permission policy; every scope is parsed once at load and matching fails closed. The
/// manifest decides what may be asked for, the consent of the user what is given.
#[derive(Debug, Clone)]
pub struct PermissionSet {
    read: Vec<Scoped<PathPattern>>,
    write: Vec<Scoped<PathPattern>>,
    exec: Vec<Scoped<ExecScope>>,
    commands: Vec<DeclaredCommand>,
    http: Vec<Scoped<UrlScope>>,
    socket: Vec<Scoped<SocketScope>>,
    shell: Vec<Scoped<UrlScope>>,
    env: HashSet<String>,
    clipboard: bool,
    shortcut: bool,
    tray: bool,
    /// Invoke items of the system context menu (`shell.contextMenu`).
    context_menu: bool,
    /// Whether a `runas` (elevating) item may be invoked (`permissions.shell.contextMenuRunas`).
    context_menu_runas: bool,
    secrets: bool,
    webcrypto: bool,
    deep_links: Vec<String>,
    autostart: bool,
    window: bool,
    /// Shared by the clones of the set: narrowing it narrows the rights of every holder.
    consent: Arc<RwLock<Consent>>,
    /// Folders no right reaches, whatever the manifest lists or the user picked.
    protected: Vec<Vec<String>>,
}

/// `$VAR` is only valid as the first path segment; its value is taken literally.
fn path_pattern(pattern: &str, vars: &PathVars) -> Result<PathPattern, AlefError> {
    match pattern.strip_prefix('$') {
        Some(rest) => {
            let end = rest.find(['/', '\\']).unwrap_or(rest.len());
            let name = &rest[..end];
            if name == "APP" && vars.app.is_none() {
                return Err(invalid("$APP is unavailable for archive applications"));
            }
            let root = vars
                .lookup(name)
                .ok_or_else(|| invalid(format!("unknown path variable ${}", &rest[..end])))?;
            PathPattern::parse(Some(root), rest[end..].trim_start_matches(['/', '\\']))
        }
        None if pattern.contains('$') => Err(invalid("path variable must be the first segment")),
        None => PathPattern::parse(None, pattern),
    }
}

/// The one refusal of every right: the same error whether the manifest does not list the right
/// or the user denied it.
pub fn refusal(permission: Permission) -> AlefError {
    AlefError::new(ErrorCode::PermissionDenied, "permission denied")
        .with_details(json!({"permission": permission.name()}))
}

fn scoped<T>(
    list: &[String],
    parse: impl Fn(&str) -> Result<T, AlefError>,
) -> Result<Vec<Scoped<T>>, AlefError> {
    list.iter()
        .map(|raw| {
            Ok(Scoped {
                raw: raw.clone(),
                scope: parse(raw)?,
            })
        })
        .collect()
}

/// The rights of one scope list.
fn rights_of<'a, T>(
    permission: &'static str,
    list: &'a [Scoped<T>],
) -> impl Iterator<Item = Right> + 'a {
    list.iter()
        .map(move |entry| Right::scoped(permission, &entry.raw))
}

/// The entry of a scope list that decides: the most restrictive decision among the entries that
/// match (the first of equally strict ones), or `None` when no entry matches (the manifest does
/// not allow it).
fn deciding<'a, T>(
    list: &'a [Scoped<T>],
    permission: &str,
    consent: &Consent,
    matches: impl Fn(&T) -> bool,
) -> Option<(Decision, &'a Scoped<T>)> {
    list.iter()
        .filter(|entry| matches(&entry.scope))
        .map(|entry| {
            (
                consent.decision(&Right::scoped(permission, &entry.raw)),
                entry,
            )
        })
        .reduce(|best, next| {
            if best.0.stricter(next.0) == best.0 {
                best
            } else {
                next
            }
        })
}

/// [`deciding`] without the entry.
fn decided<T>(
    list: &[Scoped<T>],
    permission: &str,
    consent: &Consent,
    matches: impl Fn(&T) -> bool,
) -> Option<Decision> {
    deciding(list, permission, consent, matches).map(|(decision, _)| decision)
}

impl PermissionSet {
    /// Expands variables and validates every scope; a malformed scope is `MANIFEST_INVALID`. The
    /// user has not been asked yet and nothing is held back: see [`Self::with_consent`].
    pub fn from_manifest(policy: &Permissions, vars: &PathVars) -> Result<Self, AlefError> {
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
        let commands = parse_cli(&policy.cli)?;
        Ok(Self {
            read: scoped(&policy.fs.read, |p| path_pattern(p, vars))?,
            write: scoped(&policy.fs.write, |p| path_pattern(p, vars))?,
            exec: scoped(&policy.cli.exec, ExecScope::parse)?,
            commands,
            http: scoped(&policy.net.http, UrlScope::parse)?,
            socket: scoped(&policy.net.socket, SocketScope::parse)?,
            shell: scoped(&policy.shell.open_external, UrlScope::parse)?,
            env,
            clipboard: policy.clipboard.read,
            shortcut: policy.shortcut.global,
            tray: policy.tray,
            context_menu: policy.shell.context_menu,
            context_menu_runas: policy.shell.context_menu_runas,
            secrets: policy.secrets,
            webcrypto: policy.webcrypto,
            deep_links: Vec::new(),
            autostart: policy.app.autostart,
            window: policy.window.as_ref().is_some_and(|window| window.create),
            consent: Arc::new(RwLock::new(Consent::allow_all())),
            protected: Vec::new(),
        })
    }

    /// Validates and attaches the manifest's top-level deep-link declarations. A substitute
    /// decision requires a module stand-in; it never authorizes native registration.
    pub fn with_deep_links(mut self, schemes: &[String]) -> Result<Self, AlefError> {
        super::manifest::validate_deep_links(schemes)?;
        self.deep_links = schemes.to_vec();
        Ok(self)
    }

    /// The decisions of the user replace the default of giving everything the manifest lists.
    pub fn with_consent(mut self, consent: Consent) -> Self {
        self.consent = Arc::new(RwLock::new(consent));
        self
    }

    /// Puts `folder` out of reach of every filesystem right: the decisions of the users live in the
    /// folder of the runtime, and an application that could write there would decide for itself.
    pub fn with_protected(mut self, folder: &Path) -> Self {
        if let Some(path) = canonical(folder) {
            self.protected.push(parts(&path));
        }
        self
    }

    fn is_protected(&self, components: &[String]) -> bool {
        self.protected.iter().any(|folder| {
            components.len() >= folder.len()
                && folder.iter().zip(components).all(|(a, b)| same(a, b))
        })
    }

    /// Whether the manifest allows elevating (`runas`) items of the context menu.
    pub fn context_menu_runas(&self) -> bool {
        self.context_menu_runas
    }

    fn decided_by_user(&self) -> RwLockReadGuard<'_, Consent> {
        self.consent.read().unwrap_or_else(|e| e.into_inner())
    }

    /// The decisions in force now.
    pub fn consent(&self) -> Consent {
        self.decided_by_user().clone()
    }

    /// Takes over the part of `stored` that is stricter than what is in force, and nothing else: a
    /// right the user took back applies at once, a right he gave applies from the next start, so a
    /// running application is never handed more than it was started with. `true` when something
    /// was narrowed.
    pub fn narrow(&self, stored: &Consent) -> bool {
        let mut consent = self.consent.write().unwrap_or_else(|e| e.into_inner());
        let mut narrowed = false;
        for right in self.rights() {
            if !stored.decided(&right) {
                continue;
            }
            let now = consent.decision(&right);
            let next = now.stricter(stored.decision(&right));
            if next != now {
                consent.set(right, next);
                narrowed = true;
            }
        }
        narrowed
    }

    /// Everything the manifest asks for, one right per scope entry, in a stable order: what the
    /// consent window shows and the store remembers.
    pub fn rights(&self) -> Vec<Right> {
        let mut rights: Vec<Right> = rights_of("fs.read", &self.read)
            .chain(rights_of("fs.write", &self.write))
            .chain(rights_of("cli.exec", &self.exec))
            .chain(
                self.commands
                    .iter()
                    .map(|command| Right::scoped("cli.command", &command.name)),
            )
            .chain(rights_of("net.http", &self.http))
            .chain(rights_of("net.socket", &self.socket))
            .chain(rights_of("shell.openExternal", &self.shell))
            .chain(
                self.deep_links
                    .iter()
                    .map(|scheme| Right::scoped("app.deepLinks", scheme)),
            )
            .chain(self.env.iter().map(|name| Right::scoped("app.env", name)))
            .collect();
        for (asked, name) in [
            (self.clipboard, "clipboard.read"),
            (self.shortcut, "shortcut.global"),
            (self.tray, "tray"),
            (self.context_menu, "shell.contextMenu"),
            (self.secrets, "secrets"),
            (self.webcrypto, "webcrypto"),
            (self.autostart, "app.autostart"),
            (self.window, "window.create"),
        ] {
            if asked {
                rights.push(Right::plain(name));
            }
        }
        rights.sort();
        rights.dedup();
        rights
    }

    /// The declared command called `name`; whether it may run is [`Self::check`] with
    /// `Permission::CliCommand`.
    pub fn command(&self, name: &str) -> Option<&DeclaredCommand> {
        self.commands.iter().find(|command| command.name == name)
    }

    /// What the user is told about `right` where it is listed (the description and the command
    /// line of a declared command); `None` for a right that says it all by itself.
    pub fn describe(&self, right: &Right) -> Option<String> {
        if right.permission != "cli.command" {
            return None;
        }
        self.command(right.scope.as_deref()?)
            .map(DeclaredCommand::summary)
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

    /// Checks `permission` for `target` and says what the user gave for it. A scoped permission
    /// denies a missing target; whatever the manifest does not list, or the user denied, is a
    /// uniform `PERMISSION_DENIED` naming only the permission. `Decision::Substitute` means the
    /// module is to give a stand-in, not the real thing.
    pub fn check(
        &self,
        permission: Permission,
        target: Option<&str>,
        grants: &Grants,
    ) -> Result<Decision, AlefError> {
        let flag = |asked: bool, name: &str| {
            asked.then(|| self.decided_by_user().decision(&Right::plain(name)))
        };
        let decision = match permission {
            Permission::None => return Ok(Decision::Allow),
            Permission::ClipboardRead => flag(self.clipboard, "clipboard.read"),
            Permission::ShortcutGlobal => flag(self.shortcut, "shortcut.global"),
            Permission::Tray => flag(self.tray, "tray"),
            Permission::ShellContextMenu => flag(self.context_menu, "shell.contextMenu"),
            Permission::Secrets => flag(self.secrets, "secrets"),
            Permission::WebCrypto => flag(self.webcrypto, "webcrypto"),
            Permission::AppDeepLinks => target
                .filter(|scheme| self.deep_links.iter().any(|declared| declared == *scheme))
                .map(|scheme| {
                    self.decided_by_user()
                        .decision(&Right::scoped("app.deepLinks", scheme))
                }),
            Permission::AppAutostart => flag(self.autostart, "app.autostart"),
            Permission::WindowCreate => flag(self.window, "window.create"),
            Permission::AppEnv => target.filter(|name| self.env.contains(*name)).map(|name| {
                self.decided_by_user()
                    .decision(&Right::scoped("app.env", name))
            }),
            Permission::FsRead | Permission::FsWrite => {
                return self
                    .authorize(permission, target, grants)
                    .map(|(_, decision)| decision);
            }
            Permission::CliExec => target.and_then(|t| {
                decided(&self.exec, "cli.exec", &self.decided_by_user(), |scope| {
                    scope.matches(t)
                })
            }),
            Permission::CliCommand => {
                target
                    .filter(|name| self.command(name).is_some())
                    .map(|name| {
                        self.decided_by_user()
                            .decision(&Right::scoped("cli.command", name))
                    })
            }
            Permission::NetHttp => target.and_then(|t| {
                decided(&self.http, "net.http", &self.decided_by_user(), |scope| {
                    scope.matches(t)
                })
            }),
            Permission::NetSocket => target.and_then(|t| {
                decided(
                    &self.socket,
                    "net.socket",
                    &self.decided_by_user(),
                    |scope| scope.matches(t),
                )
            }),
            Permission::ShellOpenExternal => target.and_then(|t| {
                decided(
                    &self.shell,
                    "shell.openExternal",
                    &self.decided_by_user(),
                    |scope| scope.matches(t),
                )
            }),
        };
        match decision {
            Some(Decision::Deny) | None => Err(refusal(permission)),
            Some(decision) => Ok(decision),
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
        self.authorize(permission, target, grants)
            .map(|(path, _)| path)
    }

    /// [`Self::authorize_path`] and what the user gave for the path: a path the user picked (a
    /// grant of the session) is always the real thing, the scopes of the manifest are as decided.
    pub fn authorize(
        &self,
        permission: Permission,
        target: Option<&str>,
        grants: &Grants,
    ) -> Result<(PathBuf, Decision), AlefError> {
        self.authorize_at(permission, target, grants, Reach::Through)
            .map(|authorized| (authorized.path, authorized.decision))
    }

    /// [`Self::authorize`] with the reach of the last component, and with the place of the stand-in
    /// when the user chose one.
    pub fn authorize_at(
        &self,
        permission: Permission,
        target: Option<&str>,
        grants: &Grants,
        reach: Reach,
    ) -> Result<Authorized, AlefError> {
        let (write, name, scopes) = match permission {
            Permission::FsRead => (false, "fs.read", &self.read),
            Permission::FsWrite => (true, "fs.write", &self.write),
            _ => {
                return Err(AlefError::new(
                    ErrorCode::InvalidArgument,
                    "not a filesystem permission",
                ))
            }
        };
        let resolved = target
            .and_then(|t| match reach {
                Reach::Through => canonical(Path::new(t)),
                Reach::Entry => canonical_entry(Path::new(t)),
            })
            .ok_or_else(|| refusal(permission))?;
        let components = parts(&resolved);
        if self.is_protected(&components) {
            return Err(refusal(permission));
        }
        if grants.allows(write, &components) {
            return Ok(Authorized {
                path: resolved,
                decision: Decision::Allow,
                shadow: None,
            });
        }
        match deciding(scopes, name, &self.decided_by_user(), |scope| {
            scope.matches_canonical(&components)
        }) {
            Some((Decision::Deny, _)) | None => Err(refusal(permission)),
            Some((decision, entry)) => Ok(Authorized {
                shadow: (decision == Decision::Substitute).then(|| Shadow {
                    scope: entry.raw.clone(),
                    inside: entry.scope.inside(&components),
                }),
                path: resolved,
                decision,
            }),
        }
    }

    /// [`Self::check`] with filesystem resolution on `executor`: the `symlink_metadata` and
    /// `canonicalize` work behind `fs.read`/`fs.write` never runs on the calling thread. Every
    /// other permission decides in place. Same decisions and errors as [`Self::check`]; a full
    /// executor refuses with `Busy`.
    pub async fn check_on(
        self: Arc<Self>,
        executor: &crate::session::BlockingExecutor,
        permission: Permission,
        target: Option<&str>,
        grants: &Arc<Grants>,
    ) -> Result<Decision, AlefError> {
        match permission {
            Permission::FsRead | Permission::FsWrite => {
                let target = target.map(str::to_owned);
                let grants = Arc::clone(grants);
                executor
                    .submit(move || self.check(permission, target.as_deref(), &grants))?
                    .result()
                    .await?
            }
            _ => self.check(permission, target, grants),
        }
    }

    /// [`Self::check_on`] on the shared filesystem-resolution budget.
    pub async fn check_async(
        self: Arc<Self>,
        permission: Permission,
        target: Option<&str>,
        grants: &Arc<Grants>,
    ) -> Result<Decision, AlefError> {
        self.check_on(fs_executor(), permission, target, grants)
            .await
    }

    /// [`Self::authorize_at`] with filesystem resolution on `executor`; same semantics.
    pub async fn authorize_at_on(
        self: Arc<Self>,
        executor: &crate::session::BlockingExecutor,
        permission: Permission,
        target: Option<&str>,
        grants: &Arc<Grants>,
        reach: Reach,
    ) -> Result<Authorized, AlefError> {
        let target = target.map(str::to_owned);
        let grants = Arc::clone(grants);
        executor
            .submit(move || self.authorize_at(permission, target.as_deref(), &grants, reach))?
            .result()
            .await?
    }

    /// [`Self::authorize_at_on`] on the shared budget.
    pub async fn authorize_at_async(
        self: Arc<Self>,
        permission: Permission,
        target: Option<&str>,
        grants: &Arc<Grants>,
        reach: Reach,
    ) -> Result<Authorized, AlefError> {
        self.authorize_at_on(fs_executor(), permission, target, grants, reach)
            .await
    }
}

/// Local filesystem-resolution budget for permission decisions: at most eight queued or
/// running checks. This is not a global or per-session framework limit.
fn fs_executor() -> &'static crate::session::BlockingExecutor {
    static FS: std::sync::OnceLock<crate::session::BlockingExecutor> = std::sync::OnceLock::new();
    FS.get_or_init(|| {
        crate::session::BlockingExecutor::new(
            std::num::NonZeroUsize::new(8).expect("non-zero filesystem budget"),
        )
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::num::NonZeroUsize;
    use std::sync::mpsc;
    use std::time::Duration;

    const LIMIT: Duration = Duration::from_secs(5);

    fn executor(capacity: usize) -> crate::session::BlockingExecutor {
        crate::session::BlockingExecutor::new(
            NonZeroUsize::new(capacity).expect("non-zero test limit"),
        )
    }

    fn set() -> Arc<PermissionSet> {
        // Minimal manifest: the empty fs scopes make the grants the only source of access.
        let policy: Permissions = serde_json::from_value(json!({
            "fs": {"read": [], "write": []},
            "cli": {"exec": []},
            "net": {"http": [], "socket": []},
            "shell": {"openExternal": []},
            "clipboard": {"read": false},
            "shortcut": {"global": false},
            "secrets": false,
            "app": {"env": []},
        }))
        .expect("policy");
        let root = std::env::temp_dir();
        let vars = PathVars {
            app_data: root.clone(),
            app_config: root.clone(),
            app_cache: root.clone(),
            home: root.clone(),
            documents: root.clone(),
            downloads: root.clone(),
            desktop: root.clone(),
            temp: root.clone(),
            app: Some(root),
        };
        Arc::new(PermissionSet::from_manifest(&policy, &vars).expect("permission set"))
    }

    /// A read grant for `dir` and a file inside it; the grant is shared as one `Arc`.
    fn granted(dir: &Path) -> (Arc<Grants>, PathBuf) {
        let file = dir.join("file.txt");
        std::fs::write(&file, b"").expect("file created");
        let grants = Arc::new(Grants::new());
        grants.grant_read(dir).expect("grant read");
        (grants, file)
    }

    #[tokio::test]
    async fn a_permission_decision_is_the_same_on_and_off_the_executor() {
        let set = set();
        let dir = tempfile::tempdir().expect("tempdir");
        let (grants, file) = granted(dir.path());
        let executor = executor(2);

        // (a) a granted path is allowed on both routes
        let target = file.to_str().expect("utf-8 path");
        let inline = set
            .check(Permission::FsRead, Some(target), &grants)
            .expect("inline check");
        let on_executor = tokio::time::timeout(
            LIMIT,
            set.clone()
                .check_on(&executor, Permission::FsRead, Some(target), &grants),
        )
        .await
        .expect("check_on completes")
        .expect("on-executor check");
        assert_eq!(inline, Decision::Allow);
        assert_eq!(on_executor, Decision::Allow);

        // (b) a path outside the grant is refused the same way
        let outside = std::env::temp_dir().join("alef-permissions-outside.txt");
        let outside = outside.to_str().expect("utf-8 path").to_owned();
        assert_eq!(
            set.check(Permission::FsRead, Some(&outside), &grants)
                .err()
                .expect("inline refusal")
                .code,
            ErrorCode::PermissionDenied
        );
        assert_eq!(
            tokio::time::timeout(
                LIMIT,
                set.clone()
                    .check_on(&executor, Permission::FsRead, Some(&outside), &grants,),
            )
            .await
            .expect("check_on completes")
            .err()
            .expect("on-executor refusal")
            .code,
            ErrorCode::PermissionDenied
        );

        // (c) no capability is allowed without touching the executor
        assert_eq!(
            set.check(Permission::None, None, &grants)
                .expect("no capability"),
            Decision::Allow
        );
        assert_eq!(
            tokio::time::timeout(
                LIMIT,
                set.check_on(&executor, Permission::None, None, &grants)
            )
            .await
            .expect("check_on completes")
            .expect("no capability"),
            Decision::Allow
        );
    }

    #[tokio::test]
    async fn a_path_authorization_is_the_same_on_and_off_the_executor() {
        let set = set();
        let dir = tempfile::tempdir().expect("tempdir");
        let (grants, file) = granted(dir.path());
        let executor = executor(2);
        let target = file.to_str().expect("utf-8 path");

        let inline = set
            .authorize_at(Permission::FsRead, Some(target), &grants, Reach::Through)
            .expect("inline authorization");
        let on_executor = tokio::time::timeout(
            LIMIT,
            set.clone().authorize_at_on(
                &executor,
                Permission::FsRead,
                Some(target),
                &grants,
                Reach::Through,
            ),
        )
        .await
        .expect("authorize_at_on completes")
        .expect("on-executor authorization");
        assert_eq!(inline, on_executor);
        assert_eq!(
            Some(&inline.path),
            canonical(Path::new(target)).as_ref(),
            "the authorized path is the canonical one"
        );
    }

    #[tokio::test]
    async fn a_saturated_permission_executor_refuses_the_check_with_busy() {
        let set = set();
        let dir = tempfile::tempdir().expect("tempdir");
        let (grants, file) = granted(dir.path());
        let executor = executor(1);
        let target = file.to_str().expect("utf-8 path").to_owned();

        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = mpsc::sync_channel::<()>(1);
        let blocker = executor
            .submit(move || {
                let _ = started_tx.send(());
                let _ = release_rx.recv_timeout(LIMIT);
            })
            .expect("blocker admitted");
        tokio::time::timeout(LIMIT, started_rx)
            .await
            .expect("blocker started")
            .expect("signal");

        assert_eq!(
            tokio::time::timeout(
                LIMIT,
                set.check_on(&executor, Permission::FsRead, Some(&target), &grants)
            )
            .await
            .expect("check_on completes")
            .err()
            .expect("saturated executor refuses")
            .code,
            ErrorCode::Busy
        );
        release_tx.send(()).expect("blocker waiting");
        tokio::time::timeout(LIMIT, blocker.result())
            .await
            .expect("blocker exits")
            .expect("blocker joins");
    }
}
