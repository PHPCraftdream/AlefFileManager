// SPDX-License-Identifier: MPL-2.0
// Event-loop integration follows Servo 0.6's MPL-2.0 winit example.
mod app;
mod delegate;
mod input;
mod platform;
pub(crate) mod resize_wait;
mod state;

use std::cell::RefCell;
use std::io;
use std::sync::Arc;

use crate::bridge::WindowRegistry;
use crate::ui::{UiRequest, Wake};
use crate::{Bridge, RuntimeHandle};
use alef_core::session::session::SessionManager;
use delegate::Waker;
use servo::protocol_handler::ProtocolRegistry;
use state::State;
use tokio::sync::mpsc;
use url::Url;
use winit::event_loop::EventLoop;

pub struct WindowOptions {
    pub title: String,
    pub width: f64,
    pub height: f64,
    pub icon_png: Vec<u8>,
    pub decorations: bool,
    pub resizable: bool,
}

impl WindowOptions {
    pub fn new(title: impl Into<String>, icon_png: Vec<u8>) -> Self {
        Self {
            title: title.into(),
            icon_png,
            width: 1200.0,
            height: 800.0,
            decorations: true,
            resizable: true,
        }
    }
}

struct App {
    url: Url,
    options: WindowOptions,
    registry: RefCell<Option<ProtocolRegistry>>,
    waker: Waker,
    state: Option<State>,
    error: Option<io::Error>,
    handle: RuntimeHandle,
    requests: mpsc::Receiver<UiRequest>,
    sessions: Arc<SessionManager>,
    windows: WindowRegistry,
    runtime: tokio::runtime::Handle,
    // M0.2 multiwindow spike (docs/stages/m0-spikes.md); inert unless ALEF_SPIKE_MULTIWINDOW=1.
    spike: crate::spikes::multiwindow::Spike,
}

pub fn run(bridge: &mut Bridge, options: WindowOptions) -> Result<(), Box<dyn std::error::Error>> {
    resize_wait::set_resize_trace(std::env::var("ALEF_RESIZE_TRACE").is_ok_and(|v| v == "1"));
    let event_loop = EventLoop::<Wake>::with_user_event().build()?;
    let handle = bridge.handle();
    handle.attach(event_loop.create_proxy());
    let mut app = App {
        url: bridge.entry_url.clone(),
        options,
        registry: RefCell::new(Some(bridge.take_registry()?)),
        waker: Waker(event_loop.create_proxy(), resize_wait::new_generation()),
        state: None,
        error: None,
        handle,
        requests: bridge.take_requests()?,
        sessions: bridge.sessions(),
        windows: bridge.windows(),
        runtime: tokio::runtime::Handle::current(),
        spike: crate::spikes::multiwindow::Spike::new(),
    };
    event_loop.run_app(&mut app)?;
    if let Some(error) = app.error {
        return Err(error.into());
    }
    Ok(())
}
