// SPDX-License-Identifier: MPL-2.0
// Event-loop integration follows Servo 0.6's MPL-2.0 winit example.
mod app;
mod delegate;
mod host;
mod input;
mod platform;
mod state;

use std::cell::RefCell;
use std::io;
use std::sync::Arc;

use crate::bridge::WindowRegistry;
use crate::ui::{UiRequest, Wake};
use crate::{Bridge, RuntimeHandle};
use alef_core::{
    security::window::{Length, Monitor, WindowDef, WindowPosition},
    session::session::SessionManager,
};
use delegate::Waker;
use servo::protocol_handler::ProtocolRegistry;
use state::State;
use tokio::sync::mpsc;
use url::Url;
use winit::event_loop::EventLoop;
use winit::window::Icon;

/// What the application opens at start.
pub struct WindowOptions {
    /// Title of a window that has none of its own (the application name).
    pub title: String,
    pub icon_png: Vec<u8>,
    /// The windows opened at start. The first is the main one and opens the entry document of the
    /// bridge, whatever its `url` says. Never empty.
    pub windows: Vec<WindowDef>,
}

impl WindowOptions {
    /// One window, 1200x800, centred, with the native frame.
    pub fn new(title: impl Into<String>, icon_png: Vec<u8>) -> Self {
        Self {
            title: title.into(),
            icon_png,
            windows: vec![WindowDef {
                label: "main".to_owned(),
                url: "/".to_owned(),
                width: Length::Px(1200.0),
                height: Length::Px(800.0),
                min_width: None,
                min_height: None,
                max_width: None,
                max_height: None,
                monitor: Monitor::default(),
                position: WindowPosition::default(),
                restore: false,
                title: None,
                decorations: None,
                resizable: None,
            }],
        }
    }

    /// Whether the main window has the native frame.
    pub fn decorations(mut self, enabled: bool) -> Self {
        if let Some(main) = self.windows.first_mut() {
            main.decorations = Some(enabled);
        }
        self
    }
}

struct App {
    url: Url,
    title: String,
    icon_png: Vec<u8>,
    icon: Option<Icon>,
    definitions: Vec<WindowDef>,
    registry: RefCell<Option<ProtocolRegistry>>,
    waker: Waker,
    /// Every open window, in the order they were opened.
    windows: Vec<State>,
    /// The first window has been opened (and Servo started).
    started: bool,
    /// End-to-end runs of `ALEF_E2E_QUIET=1`: windows are never shown (see `state::Pretended`).
    quiet: bool,
    /// Last id given to a close request.
    next_close: u64,
    error: Option<io::Error>,
    handle: RuntimeHandle,
    requests: mpsc::Receiver<UiRequest>,
    sessions: Arc<SessionManager>,
    ids: WindowRegistry,
    runtime: tokio::runtime::Handle,
    dialogs: host::dialogs::Dialogs,
}

pub fn run(bridge: &mut Bridge, options: WindowOptions) -> Result<(), Box<dyn std::error::Error>> {
    if options.windows.is_empty() {
        return Err(
            io::Error::new(io::ErrorKind::InvalidInput, "There is no window to open").into(),
        );
    }
    state::resize_wait::set_resize_trace(
        std::env::var("ALEF_RESIZE_TRACE").is_ok_and(|v| v == "1"),
    );
    let flag = |name: &str| std::env::var(name).is_ok_and(|value| value == "1");
    let quiet = flag("ALEF_E2E") && flag("ALEF_E2E_QUIET");
    if quiet {
        eprintln!("ALEF_E2E quiet: the windows of this run are never shown");
    }
    let event_loop = EventLoop::<Wake>::with_user_event().build()?;
    let handle = bridge.handle();
    handle.attach(event_loop.create_proxy());
    let mut app = App {
        url: bridge.entry_url.clone(),
        title: options.title,
        icon_png: options.icon_png,
        icon: None,
        definitions: options.windows,
        registry: RefCell::new(Some(bridge.take_registry()?)),
        waker: Waker(
            event_loop.create_proxy(),
            state::resize_wait::new_generation(),
        ),
        windows: Vec::new(),
        started: false,
        quiet,
        next_close: 0,
        error: None,
        handle,
        requests: bridge.take_requests()?,
        sessions: bridge.sessions(),
        ids: bridge.windows(),
        runtime: tokio::runtime::Handle::current(),
        dialogs: host::dialogs::Dialogs::new(),
    };
    event_loop.run_app(&mut app)?;
    if let Some(error) = app.error {
        return Err(error.into());
    }
    Ok(())
}
