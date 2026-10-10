// SPDX-License-Identifier: MIT OR Apache-2.0
use super::super::menu::model::{Owner, Plan};
use alef_core::registry::window::tray::{self, TrayCall};
use image::{ImageFormat, ImageReader, Limits};
use muda::MenuId;
use serde_json::Value;
use std::{
    io::{self, Cursor},
    sync::atomic::{AtomicU64, Ordering},
};

pub(crate) struct Icon {
    pub rgba: Vec<u8>,
    pub width: u32,
    pub height: u32,
}
fn invalid(message: impl Into<Box<dyn std::error::Error + Send + Sync>>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message)
}
pub(crate) fn decode(png: &[u8]) -> io::Result<Icon> {
    if png.is_empty() || png.len() > tray::ICON_BYTES_LIMIT {
        return Err(invalid("tray PNG must be 1..256 KiB"));
    }
    let mut limits = Limits::default();
    limits.max_image_width = Some(tray::ICON_SIDE_LIMIT);
    limits.max_image_height = Some(tray::ICON_SIDE_LIMIT);
    limits.max_alloc = Some(tray::ICON_DECODED_LIMIT);
    let mut reader = ImageReader::with_format(Cursor::new(png), ImageFormat::Png);
    reader.limits(limits);
    let image = reader.decode().map_err(invalid)?.into_rgba8();
    let (width, height) = image.dimensions();
    if width == 0 || height == 0 || width > tray::ICON_SIDE_LIMIT || height > tray::ICON_SIDE_LIMIT
    {
        return Err(invalid("tray PNG dimensions must be 1..256"));
    }
    Ok(Icon {
        rgba: image.into_raw(),
        width,
        height,
    })
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
