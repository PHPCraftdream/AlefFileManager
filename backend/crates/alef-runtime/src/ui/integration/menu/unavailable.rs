// SPDX-License-Identifier: MIT OR Apache-2.0
//! Where there are no native menus: muda needs GTK on Linux and a winit loop does not own it.
use crate::{bridge::EventBus, ui::Wake};
use alef_core::{registry::window::menu::MenuCall, session::session::SessionManager};
use serde_json::Value;
use std::{collections::HashMap, io, rc::Rc, time::Instant};
use winit::{
    event_loop::{ActiveEventLoop, EventLoopProxy},
    window::Window,
};

fn unsupported() -> io::Error {
    io::Error::new(
        io::ErrorKind::Unsupported,
        "Native menus need GTK, which a winit loop does not own",
    )
}

#[derive(Default)]
pub(crate) struct Menus;

impl Menus {
    pub(crate) fn prepare(
        &mut self,
        _ui: &ActiveEventLoop,
        _proxy: EventLoopProxy<Wake>,
        _windows: HashMap<u64, Rc<Window>>,
    ) -> io::Result<()> {
        Err(unsupported())
    }
    pub(crate) fn call(
        &mut self,
        _sessions: &SessionManager,
        _caller: u64,
        _target: u64,
        _call: MenuCall,
    ) -> io::Result<Value> {
        Err(unsupported())
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
