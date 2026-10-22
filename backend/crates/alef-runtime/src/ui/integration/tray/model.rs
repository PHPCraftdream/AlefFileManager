// SPDX-License-Identifier: MIT OR Apache-2.0
use super::super::menu::model::{Owner, Plan};
use alef_core::registry::window::tray::{self, TrayCall};
use muda::MenuId;
use serde_json::Value;
use std::{
    io,
    sync::atomic::{AtomicU64, Ordering},
};

pub(crate) use crate::ui::integration::tray_pure::{decode, Icon};
fn invalid(message: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
pub(crate) fn identity(call: &TrayCall, window: u64) -> (Owner, &str) {
    let (owner, token) = match call {
        TrayCall::Create { owner, token, .. }
        | TrayCall::SetIcon { owner, token, .. }
        | TrayCall::SetTooltip { owner, token, .. }
        | TrayCall::SetMenu { owner, token, .. }
        | TrayCall::Destroy { owner, token } => (*owner, token.as_str()),
    };
    (
        Owner {
            window,
            session: owner,
        },
        token,
    )
}
/// Setters must preserve the prior object on failure. Destroy releases native ownership.
pub(crate) trait Backend {
    type Object;
    fn create(&self, id: &str, icon: Icon, tooltip: &str, menu: &Plan) -> io::Result<Self::Object>;
    fn set_icon(&self, object: &mut Self::Object, icon: Icon) -> io::Result<()>;
    fn set_tooltip(&self, object: &mut Self::Object, tooltip: &str) -> io::Result<()>;
    fn set_menu(&self, object: &mut Self::Object, menu: &Plan) -> io::Result<()>;
    fn destroy(&self, object: Self::Object);
}
struct Entry<T> {
    owner: Owner,
    token: String,
    id: String,
    plan: Plan,
    object: T,
}
pub(crate) struct Table<B: Backend> {
    entries: Vec<Entry<B::Object>>,
}
impl<B: Backend> Default for Table<B> {
    fn default() -> Self {
        Self {
            entries: Vec::new(),
        }
    }
}
static GENERATION: AtomicU64 = AtomicU64::new(0);
impl<B: Backend> Table<B> {
    pub(crate) fn call(
        &mut self,
        backend: &B,
        window: u64,
        call: TrayCall,
        live: impl Fn(Owner) -> bool,
    ) -> io::Result<Value> {
        call.check().map_err(invalid)?;
        let (owner, token) = identity(&call, window);
        let token = token.to_owned();
        if !matches!(call, TrayCall::Destroy { .. }) && !live(owner) {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "Tray owner is no longer live",
            ));
        }
        let index = self
            .entries
            .iter()
            .position(|e| e.owner == owner && e.token == token);
        if let TrayCall::Create {
            icon,
            tooltip,
            menu,
            ..
        } = call
        {
            let icon = decode(&icon)?;
            if index.is_some() {
                return Err(io::Error::new(
                    io::ErrorKind::AlreadyExists,
                    "Tray token already exists",
                ));
            }
            if self.entries.iter().filter(|e| e.owner == owner).count() >= tray::SESSION_LIMIT {
                return Err(io::Error::new(
                    io::ErrorKind::WouldBlock,
                    "Tray limit reached",
                ));
            }
            let plan = Plan::build(&menu)?;
            let generation = GENERATION
                .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
                .map_err(|_| io::Error::other("Tray generation exhausted"))?;
            let id = format!("alef-runtime-tray-{}-{generation}", std::process::id());
            let object = backend.create(&id, icon, &tooltip, &plan)?;
            self.entries.push(Entry {
                owner,
                token,
                id,
                plan,
                object,
            });
        } else if let Some(index) = index {
            let entry = &mut self.entries[index];
            match call {
                TrayCall::SetIcon { icon, .. } => {
                    backend.set_icon(&mut entry.object, decode(&icon)?)?
                }
                TrayCall::SetTooltip { tooltip, .. } => {
                    backend.set_tooltip(&mut entry.object, &tooltip)?
                }
                TrayCall::SetMenu { menu, .. } => {
                    let plan = Plan::build(&menu)?;
                    backend.set_menu(&mut entry.object, &plan)?;
                    entry.plan = plan;
                }
                TrayCall::Destroy { .. } => {
                    let entry = self.entries.remove(index);
                    backend.destroy(entry.object);
                }
                TrayCall::Create { .. } => return Err(invalid("Unexpected create")),
            }
        } else if !matches!(call, TrayCall::Destroy { .. }) {
            return Err(io::Error::new(io::ErrorKind::NotFound, "Tray not found"));
        }
        Ok(Value::Null)
    }
    pub(crate) fn route(&self, id: &str, live: impl Fn(Owner) -> bool) -> Option<(Owner, &str)> {
        self.entries
            .iter()
            .find(|e| e.id == id && live(e.owner))
            .map(|e| (e.owner, e.token.as_str()))
    }
    pub(crate) fn route_menu(
        &self,
        id: &MenuId,
        live: impl Fn(Owner) -> bool,
    ) -> Option<(Owner, &str, &str)> {
        self.entries
            .iter()
            .filter(|e| live(e.owner))
            .find_map(|e| e.plan.action(id).map(|id| (e.owner, id, e.token.as_str())))
    }
    pub(crate) fn sweep(&mut self, backend: &B, live: impl Fn(Owner) -> bool) {
        for index in (0..self.entries.len()).rev() {
            if !live(self.entries[index].owner) {
                backend.destroy(self.entries.remove(index).object);
            }
        }
    }
    /// A closed window takes its trays with it.
    pub(crate) fn release_window(&mut self, backend: &B, window: u64) {
        self.sweep(backend, |owner| owner.window != window);
    }
    /// The end of the process: no icon stays in the notification area.
    pub(crate) fn release_all(&mut self, backend: &B) {
        self.sweep(backend, |_| false);
    }
    pub(crate) fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }
}
