// SPDX-License-Identifier: MIT OR Apache-2.0
use crate::{bridge::EventBus, ui::Wake};
use alef_core::{registry::window::tray::TrayCall, session::session::SessionManager};
use serde_json::Value;
use std::{io, time::Instant};
use winit::event_loop::EventLoopProxy;
#[derive(Default)]
pub(crate) struct Trays;
impl Trays {
    pub(crate) fn call(
        &mut self,
        _proxy: EventLoopProxy<Wake>,
        _sessions: &SessionManager,
        _windows: &[u64],
        _caller: u64,
        _call: TrayCall,
    ) -> io::Result<Value> {
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "Native trays need GTK, which a winit loop does not own",
        ))
    }
    pub(crate) fn tick(
        &mut self,
        _sessions: &SessionManager,
        _events: &EventBus,
        _windows: &[u64],
    ) {
    }
    pub(crate) fn release_window(&mut self, _window: u64) {}
    pub(crate) fn next_wake(&self) -> Option<Instant> {
        None
    }
    pub(crate) fn shutdown(&mut self) {}
}
