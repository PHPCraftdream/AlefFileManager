// SPDX-License-Identifier: MIT OR Apache-2.0
//! Session-owned context menus. Permission checks and all filesystem/platform work happen on the
//! blocking pool; menu resources are reserved before platform allocation.
#[cfg(all(not(windows), not(target_os = "macos")))]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(all(not(windows), not(target_os = "macos")))]
use linux as catalog;
#[cfg(target_os = "macos")]
use macos as catalog;
#[cfg(windows)]
mod catalog {
    use alef_core::{AlefError, ErrorCode};
    use std::path::Path;
    pub(super) fn open_with_apps(_: &Path) -> Vec<(String, String)> {
        Vec::new()
    }
    pub(super) fn launch_with(_: &Path, _: &str) -> Result<(), AlefError> {
        Err(AlefError::new(
            ErrorCode::NotFound,
            "Open With is unavailable on Windows fallback",
        ))
    }
}
mod rules;
#[cfg(windows)]
mod windows;
use super::{existing, ShellBackend};
use alef_core::{
    ids::ResourceId,
    registry::{command::Reply, dispatch::Registry},
    security::{consent::Decision, permissions::Permission},
    AlefError, ErrorCode,
};
use serde::Deserialize;
use serde_json::{json, Value};
use std::{
    future::Future,
    num::NonZeroUsize,
    path::{Path, PathBuf},
    pin::Pin,
    sync::{Arc, Mutex},
};

#[derive(Debug, Clone, serde::Serialize)]
pub struct ContextMenuItem {
    pub id: String,
    pub label: String,
    pub enabled: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub checked: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub default: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub separator: Option<bool>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub submenu: Option<Vec<ContextMenuItem>>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub icon: Option<Vec<u8>>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenArgs {
    paths: Vec<String>,
    extended: Option<bool>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InvokeArgs {
    handle: u64,
    id: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CloseArgs {
    handle: u64,
}
enum Backend {
    #[cfg(windows)]
    Windows(Arc<windows::StaMenu>),
    Fallback(Arc<dyn ShellBackend>),
}
struct State {
    closed: bool,
    substituted: bool,
    paths: Vec<PathBuf>,
    originals: Vec<String>,
    items: Vec<ContextMenuItem>,
    backend: Option<Backend>,
}
#[derive(Clone)]
struct MenuResource(Arc<Mutex<State>>);
impl MenuResource {
    fn close_now(&self) {
        let backend = {
            let mut s = self.0.lock().unwrap_or_else(|e| e.into_inner());
            s.closed = true;
            s.backend.take()
        };
        drop(backend);
    }
}
impl alef_core::session::Resource for MenuResource {
    fn close(self: Box<Self>) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        let this = *self;
        Box::pin(async move {
            let _ = tokio::task::spawn_blocking(move || this.close_now()).await;
        })
    }
}

fn denied() -> AlefError {
    AlefError::new(
        ErrorCode::PermissionDenied,
        "shell.contextMenu permission denied",
    )
}
fn authorized(
    ctx: &alef_core::registry::context::CallContext,
    paths: &[String],
) -> Result<(Vec<PathBuf>, bool), AlefError> {
    let mut substituted =
        ctx.permissions
            .check(Permission::ShellContextMenu, None, &ctx.grants())?
            == Decision::Substitute;
    let mut canonical = Vec::with_capacity(paths.len());
    for path in paths {
        let (resolved, decision) =
            ctx.permissions
                .authorize(Permission::FsRead, Some(path), &ctx.grants())?;
        if decision == Decision::Substitute {
            substituted = true;
        } else {
            existing(&resolved)?;
        }
        canonical.push(resolved);
    }
    if canonical.is_empty() {
        return Err(AlefError::new(
            ErrorCode::InvalidArgument,
            "context menu requires at least one path",
        ));
    }
    rules::same_parent(&canonical)?;
    Ok((canonical, substituted))
}

type MenuOpener = Arc<
    dyn Fn(
            &[PathBuf],
            Arc<dyn ShellBackend>,
            bool,
        ) -> Result<(Vec<ContextMenuItem>, Option<Backend>), AlefError>
        + Send
        + Sync,
>;

fn open_items(
    paths: &[PathBuf],
    shell: Arc<dyn ShellBackend>,
    extended: bool,
) -> Result<(Vec<ContextMenuItem>, Option<Backend>), AlefError> {
    #[cfg(windows)]
    {
        let _ = shell;
        let (menu, items) = windows::StaMenu::open(paths, extended)?;
        Ok((items, Some(Backend::Windows(Arc::new(menu)))))
    }
    #[cfg(not(windows))]
    {
        let _ = extended;
        let items = fallback_items_with(shell.as_ref(), &catalog::open_with_apps, paths);
        Ok((items, Some(Backend::Fallback(shell))))
    }
}
fn plain(id: &str, label: &str) -> ContextMenuItem {
    ContextMenuItem {
        id: id.into(),
        label: label.into(),
        enabled: true,
        checked: None,
        default: None,
        separator: None,
        submenu: None,
        icon: None,
    }
}
fn fallback_items_with(
    shell: &dyn ShellBackend,
    catalog: &dyn Fn(&Path) -> Vec<(String, String)>,
    paths: &[PathBuf],
) -> Vec<ContextMenuItem> {
    let _ = shell;
    let mut items = vec![ContextMenuItem {
        enabled: paths.first().is_some(),
        default: Some(true),
        ..plain("open", "Open")
    }];
    if let Some(path) = paths.first() {
        let apps = catalog(path);
        if !apps.is_empty() {
            items.push(ContextMenuItem {
                submenu: Some(
                    apps.into_iter()
                        .map(|(id, name)| ContextMenuItem {
                            label: format!("Open With ▸ {name}"),
                            ..plain(&format!("open-with:{id}"), "")
                        })
                        .collect(),
                ),
                ..plain("open-with", "Open With ▸")
            })
        }
    }
    items.push(plain(
        "reveal",
        if cfg!(target_os = "macos") {
            "Show in Finder"
        } else {
            "Show in Folder"
        },
    ));
    items.push(ContextMenuItem {
        separator: Some(true),
        ..plain("", "")
    });
    items.push(plain("trash", "Move to Trash"));
    items
}

fn validate_in(
    items: &[ContextMenuItem],
    id: &str,
    inherited_enabled: bool,
) -> Option<(bool, bool)> {
    items.iter().find_map(|item| {
        let enabled = inherited_enabled && item.enabled;
        if item.id == id {
            return Some((
                enabled,
                item.separator == Some(true) || item.submenu.is_some(),
            ));
        }
        item.submenu
            .as_deref()
            .and_then(|children| validate_in(children, id, enabled))
    })
}
fn validate_item(items: &[ContextMenuItem], id: &str) -> Result<(), AlefError> {
    let (enabled, nonleaf) = validate_in(items, id, true)
        .ok_or_else(|| AlefError::new(ErrorCode::InvalidArgument, "unknown menu item id"))?;
    if !enabled || nonleaf {
        return Err(AlefError::new(
            ErrorCode::PermissionDenied,
            "context menu item is not actionable",
        ));
    }
    Ok(())
}
fn invoke_state(
    state: Arc<Mutex<State>>,
    id: String,
    permissions: Arc<alef_core::security::permissions::PermissionSet>,
    grants: Arc<alef_core::security::permissions::Grants>,
) -> Result<(), AlefError> {
    let s = state.lock().unwrap_or_else(|e| e.into_inner());
    if s.closed {
        return Err(AlefError::new(ErrorCode::NotFound, "context menu closed"));
    }
    let originals = s.originals.clone();
    let permission_decision = permissions.check(Permission::ShellContextMenu, None, &grants)?;
    if permission_decision != Decision::Allow {
        return Err(denied());
    }
    if s.substituted {
        return Err(denied());
    }
    let spec = rules::parse_id(&id)?;
    validate_item(&s.items, &id)?;
    let paths = s.paths.clone();
    authorized_from(&permissions, &grants, &paths, false)?;
    let check_paths = paths
        .iter()
        .cloned()
        .chain(originals.iter().map(PathBuf::from))
        .collect::<Vec<_>>();
    match s
        .backend
        .as_ref()
        .ok_or_else(|| AlefError::new(ErrorCode::NotFound, "context menu closed"))?
    {
        #[cfg(windows)]
        Backend::Windows(menu) => {
            if let rules::InvokeSpec::WindowsOffset(offset) = spec {
                menu.invoke(offset, &check_paths, permissions.context_menu_runas())
            } else {
                Err(AlefError::new(
                    ErrorCode::InvalidArgument,
                    "invalid Windows command",
                ))
            }
        }
        Backend::Fallback(shell) => match spec {
            rules::InvokeSpec::Fallback("open") => {
                rules::verb_refused("open", &check_paths, false)?;
                paths.iter().try_for_each(|p| shell.open_path(p))
            }
            rules::InvokeSpec::Fallback("reveal") => shell.show_in_folder(&paths[0]),
            rules::InvokeSpec::Fallback("trash") => {
                authorized_from(&permissions, &grants, &paths, true)?;
                paths.iter().try_for_each(|p| shell.trash(p))
            }
            rules::InvokeSpec::OpenWith(app) => {
                rules::verb_refused("open-with", &check_paths, false)?;
                paths.iter().try_for_each(|p| catalog::launch_with(p, &app))
            }
            _ => Err(AlefError::new(
                ErrorCode::InvalidArgument,
                "unknown menu item id",
            )),
        },
    }
}
fn authorized_from(
    p: &alef_core::security::permissions::PermissionSet,
    g: &alef_core::security::permissions::Grants,
    paths: &[PathBuf],
    write: bool,
) -> Result<(), AlefError> {
    p.check(Permission::ShellContextMenu, None, g)?;
    for path in paths {
        let (rechecked, d) = p.authorize(
            if write {
                Permission::FsWrite
            } else {
                Permission::FsRead
            },
            Some(&path.to_string_lossy()),
            g,
        )?;
        if rechecked != *path {
            return Err(denied());
        }
        if d != Decision::Allow {
            return Err(denied());
        }
        existing(path)?;
    }
    Ok(())
}

pub(crate) fn register(
    registry: &mut Registry,
    shell: Arc<dyn ShellBackend>,
) -> Result<(), AlefError> {
    let permission_executor = Arc::new(alef_core::session::executor::BlockingExecutor::new(
        NonZeroUsize::new(64).unwrap(),
    ));
    let work_executor = Arc::new(alef_core::session::executor::BlockingExecutor::new(
        NonZeroUsize::new(64).unwrap(),
    ));
    register_with(
        registry,
        shell,
        Arc::new(open_items),
        permission_executor,
        work_executor,
    )
}

fn register_with(
    registry: &mut Registry,
    shell: Arc<dyn ShellBackend>,
    opener: MenuOpener,
    permission_executor: Arc<alef_core::session::executor::BlockingExecutor>,
    work_executor: Arc<alef_core::session::executor::BlockingExecutor>,
) -> Result<(), AlefError> {
    let open_executor = work_executor.clone();
    registry
        .command::<OpenArgs>("shell.contextMenu")?
        .handler(move |ctx, args| {
            let opener = opener.clone();
            let permission_executor = permission_executor.clone();
            let work_executor = open_executor.clone();
            let shell = shell.clone();
            async move {
                let transaction = tokio::spawn(async move {
                    let paths = args.paths.clone();
                    let originals = args.paths;
                    let extended = args.extended.unwrap_or(false);
                    let state = Arc::new(Mutex::new(State {
                        closed: false,
                        substituted: false,
                        paths: Vec::new(),
                        originals: Vec::new(),
                        items: Vec::new(),
                        backend: None,
                    }));
                    let handle = ctx
                        .resources()
                        .insert(Box::new(MenuResource(state.clone())))?;

                    // This transaction survives caller cancellation; on success the resource remains session-owned.
                    let result = async {
                        let ctx2 = ctx.clone();
                        let (canonical, substituted) = permission_executor
                            .submit(move || authorized(&ctx2, &paths))?
                            .result()
                            .await??;
                        let work_state = state.clone();
                        let session = ctx.session.clone();
                        let items = work_executor
                            .submit(move || {
                                if !session.is_open() {
                                    return Err(AlefError::new(
                                        ErrorCode::Closed,
                                        "session closed during menu creation",
                                    ));
                                }
                                if work_state.lock().unwrap_or_else(|e| e.into_inner()).closed {
                                    return Err(AlefError::new(
                                        ErrorCode::Closed,
                                        "menu closed during creation",
                                    ));
                                }
                                let (items, backend) = if substituted {
                                    (Vec::new(), None)
                                } else {
                                    opener(&canonical, shell, extended)?
                                };
                                let mut state =
                                    work_state.lock().unwrap_or_else(|e| e.into_inner());
                                if state.closed || !session.is_open() {
                                    drop(state);
                                    drop(backend);
                                    return Err(AlefError::new(
                                        ErrorCode::Closed,
                                        "menu closed during creation",
                                    ));
                                }
                                state.paths = canonical;
                                state.originals = originals;
                                state.items = items.clone();
                                state.backend = backend;
                                state.substituted = substituted;
                                Ok(items)
                            })?
                            .result()
                            .await??;
                        if !ctx.session.is_open() {
                            return Err(AlefError::new(
                                ErrorCode::Closed,
                                "session closed during menu creation",
                            ));
                        }
                        Ok(items)
                    }
                    .await;
                    if result.is_err() {
                        let _ = ctx.resources().remove(handle);
                    }
                    result.map(|items| Reply::Json(json!({"handle":handle.0,"items":items})))
                });
                transaction
                    .await
                    .map_err(|e| AlefError::new(ErrorCode::Internal, e.to_string()))?
            }
        })?;
    let invoke_executor = work_executor.clone();
    registry
        .command::<InvokeArgs>("shell.contextMenu.invoke")?
        .handler(move |ctx, args| {
            let work_executor = invoke_executor.clone();
            async move {
                let menu = ctx
                    .resources()
                    .with_as::<MenuResource, _>(ResourceId(args.handle), Clone::clone)?;
                let p = ctx.permissions.clone();
                let g = ctx.grants();
                work_executor
                    .submit(move || invoke_state(menu.0, args.id, p, g))?
                    .result()
                    .await??;
                Ok(Reply::Json(Value::Null))
            }
        })?;
    registry
        .command::<CloseArgs>("shell.contextMenu.close")?
        .handler(|ctx, args| async move {
            let menu = ctx
                .resources()
                .with_as::<MenuResource, _>(ResourceId(args.handle), Clone::clone)?;
            let copy = menu.clone();
            tokio::task::spawn_blocking(move || copy.close_now())
                .await
                .map_err(|e| AlefError::new(ErrorCode::Internal, e.to_string()))?;
            ctx.resources().remove(ResourceId(args.handle))?;
            Ok(Reply::Json(Value::Null))
        })?;
    Ok(())
}

#[cfg(test)]
#[path = "registry_tests.rs"]
mod registry_tests;
#[cfg(test)]
#[path = "tests.rs"]
mod tests;
