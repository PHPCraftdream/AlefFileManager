// SPDX-License-Identifier: MIT OR Apache-2.0
mod model;
mod native;
#[cfg(test)]
mod tests;
use super::{forward::Forwarder, menu::model::Owner};
use crate::{
    bridge::EventBus,
    ui::{event_json, Wake},
};
use alef_core::{registry::window::tray::TrayCall, session::session::SessionManager};
use model::Table;
use native::Native;
use serde_json::{json, Value};
use std::{
    io,
    time::{Duration, Instant},
};
use tray_icon::{MouseButton, MouseButtonState, TrayIconEvent};
use winit::event_loop::EventLoopProxy;
const SWEEP: Duration = Duration::from_millis(250);
fn live(sessions: &SessionManager, windows: &[u64], owner: Owner) -> bool {
    windows.contains(&owner.window)
        && sessions
            .current(owner.window)
            .is_some_and(|s| s.id() == owner.session)
}
#[derive(Default)]
pub(crate) struct Trays {
    table: Table<Native>,
    forward: Option<Forwarder<TrayIconEvent>>,
    next_sweep: Option<Instant>,
}
impl Trays {
    pub(crate) fn call(
        &mut self,
        proxy: EventLoopProxy<Wake>,
        sessions: &SessionManager,
        windows: &[u64],
        caller: u64,
        call: TrayCall,
    ) -> io::Result<Value> {
        if super::spike_owns_hotkeys() {
            return Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "The integration spike owns native trays",
            ));
        }
        if self.forward.is_none() {
            self.forward = Some(Forwarder::start(
                TrayIconEvent::receiver().clone(),
                move || proxy.send_event(Wake).is_ok(),
            )?);
        }
        let result = self
            .table
            .call(&Native, caller, call, |o| live(sessions, windows, o));
        if !self.table.is_empty() {
            self.next_sweep
                .get_or_insert_with(|| Instant::now() + SWEEP);
        }
        result
    }
    pub(crate) fn route_menu(
        &self,
        id: &muda::MenuId,
        sessions: &SessionManager,
        events: &EventBus,
        windows: &[u64],
    ) {
        if let Some((owner, id, token)) = self.table.route_menu(id, |o| live(sessions, windows, o))
        {
            publish(
                events,
                owner,
                "runtime.tray.menu-click",
                json!({"owner":owner.session.0,"id":id,"token":token}),
            );
        }
    }
    pub(crate) fn tick(&mut self, sessions: &SessionManager, events: &EventBus, windows: &[u64]) {
        let now = Instant::now();
        if self.next_sweep.is_some_and(|due| now >= due) {
            self.table.sweep(&Native, |o| live(sessions, windows, o));
            self.next_sweep = (!self.table.is_empty()).then_some(now + SWEEP);
        }
        if let Some(forward) = &self.forward {
            for event in forward.drain() {
                if let Some((id, name, button)) = click(&event) {
                    if let Some((owner, token)) =
                        self.table.route(id, |o| live(sessions, windows, o))
                    {
                        publish(
                            events,
                            owner,
                            name,
                            json!({"owner":owner.session.0,"token":token,"button":button}),
                        );
                    }
                }
            }
        }
    }
    pub(crate) fn release_window(&mut self, window: u64) {
        self.table.release_window(&Native, window);
    }
    pub(crate) fn next_wake(&self) -> Option<Instant> {
        if self.table.is_empty() {
            None
        } else {
            self.next_sweep
        }
    }
    pub(crate) fn shutdown(&mut self) {
        self.table.release_all(&Native);
        self.forward.take();
        self.next_sweep = None;
    }
}
impl Drop for Trays {
    fn drop(&mut self) {
        self.shutdown();
    }
}
fn publish(events: &EventBus, owner: Owner, name: &str, payload: Value) {
    if let Ok(json) = event_json(name, &payload) {
        events.publish_session(owner.window, owner.session, &json);
    }
}
fn click(event: &TrayIconEvent) -> Option<(&str, &'static str, &'static str)> {
    let (id, button, double) = match event {
        TrayIconEvent::Click {
            id,
            button,
            button_state: MouseButtonState::Up,
            ..
        } => (id, button, false),
        TrayIconEvent::DoubleClick { id, button, .. } => (id, button, true),
        _ => return None,
    };
    let button = match button {
        MouseButton::Left => "left",
        MouseButton::Right => "right",
        MouseButton::Middle => "middle",
    };
    let name = if double {
        "runtime.tray.double-click"
    } else if button == "right" {
        "runtime.tray.right-click"
    } else {
        "runtime.tray.click"
    };
    Some((id.as_ref(), name, button))
}
