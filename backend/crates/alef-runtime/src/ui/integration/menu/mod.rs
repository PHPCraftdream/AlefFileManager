// SPDX-License-Identifier: MIT OR Apache-2.0
//! UI-thread menus on Windows and macOS (NSApp on the main thread); elsewhere see `unavailable`.
pub(crate) mod model;
pub(crate) mod native;
#[cfg(test)]
mod tests;

use super::forward::Forwarder;
use crate::{
    bridge::EventBus,
    ui::{event_json, Wake},
};
use alef_core::{
    registry::window::menu::{MenuCall, MenuItem},
    session::session::SessionManager,
};
use model::{Owner, Plan, Table};
use muda::MenuEvent;
use native::Native;
use serde_json::Value;
use std::{
    collections::HashMap,
    io,
    rc::Rc,
    time::{Duration, Instant},
};
use winit::{
    event_loop::{ActiveEventLoop, EventLoopProxy},
    window::Window,
};
const SWEEP: Duration = Duration::from_millis(250);
const CHOICE: Duration = Duration::from_millis(500);

#[cfg(target_os = "windows")]
pub(crate) type Accelerators = Rc<std::cell::RefCell<HashMap<isize, native::AcceleratorHandle>>>;

fn live_owner(sessions: &SessionManager, windows: &[u64], owner: Owner) -> bool {
    windows.contains(&owner.window)
        && sessions
            .current(owner.window)
            .is_some_and(|s| s.id() == owner.session)
}

#[derive(Default)]
pub(crate) struct Menus {
    backend: Option<Native>,
    forward: Option<Forwarder<MenuEvent>>,
    table: Table<Native>,
    next_sweep: Option<Instant>,
    #[cfg(target_os = "windows")]
    pub(crate) accelerators: Accelerators,
}
impl Menus {
    fn ensure(
        &mut self,
        ui: &ActiveEventLoop,
        proxy: EventLoopProxy<Wake>,
        windows: HashMap<u64, Rc<Window>>,
    ) -> io::Result<()> {
        if super::spike_owns_hotkeys() {
            return Err(model::unsupported(
                "The integration spike owns native menus",
            ));
        }
        if let Some(backend) = &mut self.backend {
            backend.update(windows);
        } else {
            let backend = Native::new(ui, windows);
            let forward = Forwarder::start(MenuEvent::receiver().clone(), move || {
                proxy.send_event(Wake).is_ok()
            })?;
            self.backend = Some(backend);
            self.forward = Some(forward);
        }
        Ok(())
    }
    pub(crate) fn call(
        &mut self,
        sessions: &SessionManager,
        caller: u64,
        target_window: u64,
        call: MenuCall,
    ) -> io::Result<Value> {
        let owner = match &call {
            MenuCall::SetApplication { owner, .. }
            | MenuCall::SetWindow { owner, .. }
            | MenuCall::Popup { owner, .. }
            | MenuCall::Release { owner } => Owner {
                window: caller,
                session: *owner,
            },
        };
        let backend = self.backend.as_ref().expect("ensured");
        if !matches!(&call, MenuCall::Release { .. })
            && !sessions
                .current(caller)
                .is_some_and(|s| s.id() == owner.session)
        {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "Menu owner is no longer live",
            ));
        }
        let result = match call {
            MenuCall::SetApplication { items, .. } => self.table.set(
                backend,
                owner,
                model::target(true, caller, target_window)?,
                &items,
            ),
            MenuCall::SetWindow { items, .. } => self.table.set(
                backend,
                owner,
                model::target(false, caller, target_window)?,
                &items,
            ),
            MenuCall::Popup { items, x, y, .. } => self.popup(caller, &items, x, y),
            MenuCall::Release { .. } => self.table.release(backend, owner),
        };
        if !self.table.is_empty() {
            self.next_sweep
                .get_or_insert_with(|| Instant::now() + SWEEP);
        }
        self.refresh_accelerators();
        result
    }
    /// A context menu of the window `window`; the call returns once the menu is closed.
    fn popup(
        &self,
        window: u64,
        items: &[MenuItem],
        x: Option<f64>,
        y: Option<f64>,
    ) -> io::Result<Value> {
        let at = model::position(x, y)?;
        let plan = Plan::build(items)?;
        if items.is_empty() {
            return Ok(Value::Null);
        }
        let backend = self.backend.as_ref().expect("ensured");
        if !backend.popup(window, &plan, at)? {
            return Ok(Value::Null);
        }
        // The choice is sent while the menu closes: it is in the queue of the forwarder in no time.
        let forward = self.forward.as_ref().expect("ensured");
        Ok(
            model::chosen(&plan, || forward.next_within(CHOICE).map(|event| event.id))
                .map_or(Value::Null, Value::from),
        )
    }
    pub(crate) fn prepare(
        &mut self,
        ui: &ActiveEventLoop,
        proxy: EventLoopProxy<Wake>,
        windows: HashMap<u64, Rc<Window>>,
    ) -> io::Result<()> {
        self.ensure(ui, proxy, windows)
    }
    fn refresh_accelerators(&self) {
        #[cfg(target_os = "windows")]
        {
            *self.accelerators.borrow_mut() = self
                .table
                .active_trees()
                .filter_map(|t| t.accelerator())
                .map(|a| (a.hwnd, a))
                .collect();
        }
    }
    fn sweep_due(&mut self, now: Instant) -> bool {
        sweep_due(self.table.is_empty(), &mut self.next_sweep, now)
    }
    pub(crate) fn tick(&mut self, sessions: &SessionManager, events: &EventBus, windows: &[u64]) {
        let live = |owner| live_owner(sessions, windows, owner);
        if self.sweep_due(Instant::now()) {
            if let Some(backend) = &self.backend {
                self.table.sweep(backend, live);
            }
            self.refresh_accelerators();
        }
        if let Some(forward) = &self.forward {
            for event in forward.drain() {
                if let Some((owner, id)) = self.table.route(&event.id, live) {
                    if let Ok(json) = event_json(
                        "runtime.menu.clicked",
                        &serde_json::json!({"owner": owner.session.0, "id": id}),
                    ) {
                        events.publish_session(owner.window, owner.session, &json);
                    }
                }
            }
        }
    }
    pub(crate) fn release_window(&mut self, window: u64) {
        if let Some(backend) = &mut self.backend {
            self.table.release_window(backend, window);
            backend.forget(window);
        }
        self.refresh_accelerators();
    }
    pub(crate) fn next_wake(&self) -> Option<Instant> {
        if self.table.is_empty() {
            None
        } else {
            self.next_sweep
        }
    }
    pub(crate) fn shutdown(&mut self) {
        #[cfg(target_os = "windows")]
        self.accelerators.borrow_mut().clear();
        if let Some(backend) = &self.backend {
            self.table.sweep(backend, |_| false);
        }
        self.forward.take();
        self.table = Table::default();
        self.backend.take();
        self.next_sweep = None;
    }
}
impl Drop for Menus {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn sweep_due(empty: bool, next: &mut Option<Instant>, now: Instant) -> bool {
    if empty {
        *next = None;
        return false;
    }
    let deadline = next.get_or_insert(now + SWEEP);
    if now < *deadline {
        return false;
    }
    *deadline = now + SWEEP;
    true
}
