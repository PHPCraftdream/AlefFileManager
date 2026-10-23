// SPDX-License-Identifier: MIT OR Apache-2.0
//! The Windows/macOS broker tray backend: a windowless winit 0.30 event loop driven
//! by an `ApplicationHandler` (same shape as `window/app.rs`), owning the
//! tray-icon/muda icon and menu. Menu and icon events arrive through the global
//! tray-icon/muda receivers, forwarded into the loop via `EventLoopProxy`, exactly
//! like the document-bound native backend in `ui/integration/tray`.

use super::{pure, Prepared, Shutdown};
use crate::ui::integration::forward::Forwarder;
use std::{io, sync::Arc};
use tray_icon::{TrayIconBuilder, TrayIconEvent};
use winit::{
    application::ApplicationHandler,
    event_loop::{ActiveEventLoop, ControlFlow, EventLoop},
};

/// User events driving the windowless loop.
#[derive(Clone, Copy, Debug)]
enum Wake {
    /// Drain the forwarded tray/menu events.
    Drain,
    /// The stop handle asked to exit.
    Exit,
}

struct Loop {
    prepared: Prepared,
    shutdown: Arc<Shutdown>,
    icon_events: Option<Forwarder<TrayIconEvent>>,
    menu_events: Option<Forwarder<muda::MenuEvent>>,
}

impl ApplicationHandler<Wake> for Loop {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        event_loop.set_control_flow(ControlFlow::Wait);
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: Wake) {
        match event {
            Wake::Exit => event_loop.exit(),
            Wake::Drain => {
                if let Some(menu_events) = &mut self.menu_events {
                    for event in menu_events.drain() {
                        if let Some(command) = pure::command(event.id.0.as_str()) {
                            let _ = self.prepared.commands.send(command);
                        }
                    }
                }
                // Icon clicks open the menu (left click is configured off); the
                // forwarded icon events carry no command and are drained away.
                if let Some(icon_events) = &self.icon_events {
                    icon_events.drain().for_each(|_| {});
                }
                if self.shutdown.requested() {
                    event_loop.exit();
                }
            }
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if self.shutdown.requested() {
            event_loop.exit();
        }
    }

    /// The broker loop owns no windows, so this never fires.
    fn window_event(
        &mut self,
        _event_loop: &ActiveEventLoop,
        _window_id: winit::window::WindowId,
        _event: winit::event::WindowEvent,
    ) {
    }
}

fn icon(prepared: &Prepared) -> io::Result<tray_icon::Icon> {
    tray_icon::Icon::from_rgba(
        prepared.icon.rgba.clone(),
        prepared.icon.width,
        prepared.icon.height,
    )
    .map_err(io::Error::other)
}

fn menu(prepared: &Prepared) -> io::Result<muda::Menu> {
    let items: Vec<muda::MenuItem> = pure::entries(&prepared.show_apps_label, &prepared.quit_label)
        .iter()
        .map(|entry| muda::MenuItem::with_id(entry.id, &entry.label, true, None))
        .collect();
    let references: Vec<&dyn muda::IsMenuItem> = items
        .iter()
        .map(|item| item as &dyn muda::IsMenuItem)
        .collect();
    muda::Menu::with_items(&references).map_err(io::Error::other)
}

pub(super) fn run(prepared: Prepared, shutdown: Arc<Shutdown>) -> io::Result<()> {
    let mut builder = EventLoop::<Wake>::with_user_event();
    // The broker is a background process: no Dock icon, no regular activation.
    #[cfg(target_os = "macos")]
    {
        use winit::platform::macos::{ActivationPolicy, EventLoopBuilderExtMacOS};
        builder.with_activation_policy(ActivationPolicy::Accessory);
    }
    let event_loop = builder.build().map_err(io::Error::other)?;
    let proxy = event_loop.create_proxy();
    // Arm before touching the tray: an early exit() must still stop `run`.
    shutdown.arm({
        let proxy = proxy.clone();
        Box::new(move || {
            let _ = proxy.send_event(Wake::Exit);
        })
    });
    if shutdown.requested() {
        return Ok(());
    }
    let tray = TrayIconBuilder::new()
        .with_id("alef-broker")
        .with_icon(icon(&prepared)?)
        .with_tooltip(&prepared.tooltip)
        .with_menu(Box::new(menu(&prepared)?))
        .with_menu_on_left_click(false)
        .build()
        .map_err(io::Error::other)?;
    // Global receivers feed the loop through the proxy; `Drain` reads them.
    let icon_events = Some(Forwarder::start(TrayIconEvent::receiver().clone(), {
        let proxy = proxy.clone();
        move || proxy.send_event(Wake::Drain).is_ok()
    })?);
    let menu_events = Some(Forwarder::start(muda::MenuEvent::receiver().clone(), {
        let proxy = proxy.clone();
        move || proxy.send_event(Wake::Drain).is_ok()
    })?);
    let mut state = Loop {
        prepared,
        shutdown,
        icon_events,
        menu_events,
    };
    let result = event_loop.run_app(&mut state).map_err(io::Error::other);
    // The loop is over: stop the forwarders and remove the icon.
    drop(state.menu_events.take());
    drop(state.icon_events.take());
    drop(tray);
    result
}
