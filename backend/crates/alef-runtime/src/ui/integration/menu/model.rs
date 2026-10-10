// SPDX-License-Identifier: MIT OR Apache-2.0
use alef_core::{
    ids::SessionId,
    registry::window::menu::{check_items, MenuItem, MenuKind},
};
use muda::{accelerator::Accelerator, MenuId};
use serde_json::Value;
use std::{
    collections::HashMap,
    io,
    sync::atomic::{AtomicU64, Ordering},
};

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct Owner {
    pub window: u64,
    pub session: SessionId,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Target {
    Window(u64),
    Application,
}

pub(crate) fn target(application: bool, caller: u64, window: u64) -> io::Result<Target> {
    if cfg!(target_os = "macos") {
        if application {
            Ok(Target::Application)
        } else {
            Err(unsupported("macOS window menus are not supported"))
        }
    } else {
        Ok(Target::Window(if application { caller } else { window }))
    }
}
pub(super) fn unsupported(message: &str) -> io::Error {
    io::Error::new(io::ErrorKind::Unsupported, message)
}

pub(crate) struct Node {
    pub id: MenuId,
    pub item: MenuItem,
    pub accelerator: Option<Accelerator>,
    pub children: Vec<Node>,
}
pub(crate) struct Plan {
    pub root: MenuId,
    pub nodes: Vec<Node>,
    actions: HashMap<MenuId, String>,
}
// Process-wide, including failed builds and newly constructed tables; never wrap.
static GENERATION: AtomicU64 = AtomicU64::new(0);
impl Plan {
    /// The id the page gave to an item that can be chosen; `None` for any other.
    pub(crate) fn action(&self, id: &MenuId) -> Option<&str> {
        self.actions.get(id).map(String::as_str)
    }
    pub(crate) fn build(items: &[MenuItem]) -> io::Result<Self> {
        check_items(items).map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
        let generation = GENERATION
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
            .map_err(|_| io::Error::other("Menu generation exhausted"))?
            + 1;
        let prefix = format!("alef-runtime-menu-{}-{generation}", std::process::id());
        fn visit(
            items: &[MenuItem],
            enabled: bool,
            prefix: &str,
            ordinal: &mut u64,
            actions: &mut HashMap<MenuId, String>,
        ) -> io::Result<Vec<Node>> {
            items
                .iter()
                .map(|item| {
                    *ordinal += 1; // check_items bounds the total to 256.
                    let id = MenuId::new(format!("{prefix}-{ordinal}"));
                    let enabled = enabled && item.enabled != Some(false);
                    let accelerator = item
                        .accelerator
                        .as_deref()
                        .map(str::parse::<Accelerator>)
                        .transpose()
                        .map_err(|e| io::Error::new(io::ErrorKind::InvalidInput, e))?;
                    if enabled
                        && item.role.is_none()
                        && matches!(item.effective_kind(), MenuKind::Normal | MenuKind::Check)
                    {
                        actions.insert(id.clone(), item.id.clone().expect("validated custom id"));
                    }
                    let children = visit(
                        item.items.as_deref().unwrap_or_default(),
                        enabled,
                        prefix,
                        ordinal,
                        actions,
                    )?;
                    let mut item = item.clone();
                    item.items = None; // children live only in the bounded plan, no duplicate subtrees.
                    item.enabled = Some(enabled);
                    Ok(Node {
                        id,
                        item,
                        accelerator,
                        children,
                    })
                })
                .collect()
        }
        let mut actions = HashMap::new();
        let nodes = visit(items, true, &prefix, &mut 0, &mut actions)?;
        Ok(Self {
            root: MenuId::new(format!("{prefix}-0")),
            nodes,
            actions,
        })
    }
}

/// On replacement failure, `active` reports whether rollback preserved prior.
/// remove must keep the tree alive on failure. No native types cross threads.
pub(crate) trait Backend {
    type Tree;
    fn build(&self, plan: &Plan) -> io::Result<Self::Tree>;
    fn replace(
        &self,
        target: Target,
        next: &mut Self::Tree,
        prior: Option<&mut Self::Tree>,
    ) -> io::Result<()>;
    fn active(&self, _tree: &Self::Tree) -> bool {
        true
    }
    fn remove(&self, target: Target, tree: &mut Self::Tree) -> io::Result<()>;
}
struct Entry<T> {
    owner: Owner,
    target: Target,
    plan: Plan,
    tree: T,
    retired: bool,
}
pub(crate) struct Table<B: Backend> {
    entries: Vec<Entry<B::Tree>>,
}
impl<B: Backend> Default for Table<B> {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
        }
    }
}
impl<B: Backend> Table<B> {
    pub(crate) fn set(
        &mut self,
        backend: &B,
        owner: Owner,
        target: Target,
        items: &[MenuItem],
    ) -> io::Result<Value> {
        let index = self.entries.iter().position(|e| e.target == target);
        if items.is_empty() {
            // Clearing, just like resource release, cannot remove a different document's menu.
            if let Some(index) = index.filter(|i| self.entries[*i].owner == owner) {
                self.remove(backend, index)?;
            }
            return Ok(Value::Null);
        }
        let plan = Plan::build(items)?;
        let mut tree = backend.build(&plan)?;
        if let Err(error) =
            backend.replace(target, &mut tree, index.map(|i| &mut self.entries[i].tree))
        {
            if let Some(i) = index {
                if !backend.active(&self.entries[i].tree) {
                    self.entries[i].retired = true;
                }
            }
            return Err(error);
        }
        let entry = Entry {
            owner,
            target,
            plan,
            tree,
            retired: false,
        };
        if let Some(i) = index {
            self.entries[i] = entry;
        } else {
            self.entries.push(entry);
        }
        Ok(Value::Null)
    }
    fn remove(&mut self, backend: &B, index: usize) -> io::Result<()> {
        let entry = &mut self.entries[index];
        entry.retired = true; // Even failed detach cannot route events; sweep retries.
        backend.remove(entry.target, &mut entry.tree)?;
        self.entries.remove(index);
        Ok(())
    }
    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
    #[cfg(target_os = "windows")]
    pub(crate) fn active_trees(&self) -> impl Iterator<Item = &B::Tree> {
        self.entries.iter().filter(|e| !e.retired).map(|e| &e.tree)
    }
    pub(crate) fn release(&mut self, backend: &B, owner: Owner) -> io::Result<Value> {
        let mut error = None;
        for i in (0..self.entries.len()).rev() {
            if self.entries[i].owner == owner {
                if let Err(e) = self.remove(backend, i) {
                    error = Some(e);
                }
            }
        }
        error.map_or(Ok(Value::Null), Err)
    }
    pub(crate) fn release_window(&mut self, backend: &B, window: u64) {
        self.sweep_matching(backend, |owner, target| {
            owner.window == window || target == Target::Window(window)
        });
    }
    pub(crate) fn sweep(&mut self, backend: &B, live: impl Fn(Owner) -> bool) {
        self.sweep_matching(backend, |owner, _| !live(owner));
    }
    fn sweep_matching(&mut self, backend: &B, stale: impl Fn(Owner, Target) -> bool) {
        for i in (0..self.entries.len()).rev() {
            let e = &self.entries[i];
            if e.retired || stale(e.owner, e.target) {
                let _ = self.remove(backend, i);
            }
        }
    }
    pub(crate) fn route(&self, id: &MenuId, live: impl Fn(Owner) -> bool) -> Option<(Owner, &str)> {
        self.entries
            .iter()
            .filter(|e| !e.retired && live(e.owner))
            .find_map(|e| e.plan.actions.get(id).map(|id| (e.owner, id.as_str())))
    }
}
/// Both coordinates or none: the menu opens at the pointer without them.
pub(crate) fn position(x: Option<f64>, y: Option<f64>) -> io::Result<Option<(f64, f64)>> {
    match (x, y) {
        (Some(x), Some(y)) => Ok(Some((x, y))),
        (None, None) => Ok(None),
        _ => Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "x and y come together or not at all",
        )),
    }
}

/// The id the page gave to what the user chose: of `events`, in order, the first that names an
/// item of the plan that can be chosen. Anything else is not for this menu.
pub(crate) fn chosen(plan: &Plan, mut events: impl FnMut() -> Option<MenuId>) -> Option<String> {
    while let Some(id) = events() {
        if let Some(action) = plan.action(&id) {
            return Some(action.to_owned());
        }
    }
    None
}
