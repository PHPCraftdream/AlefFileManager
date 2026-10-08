// SPDX-License-Identifier: MIT OR Apache-2.0
//! The modes without a window (a console utility, a service): the entry document of the application runs
//! in a hidden WebView on a software rendering context, with no window and no winit event loop. Nothing is
//! drawn. The requests that need a window (`window.*`, `dialog.*`) answer `NOT_AVAILABLE`.
use std::{
    cell::Cell,
    error::Error,
    io,
    rc::Rc,
    sync::{Arc, Condvar, Mutex},
    time::Duration,
};

use alef_core::registry::host::Host;
use servo::{
    ConsoleLogLevel, EventLoopWaker, NavigationRequest, Opts, RenderingContext, ServoBuilder,
    SoftwareRenderingContext, WebResourceLoad, WebView, WebViewBuilder, WebViewDelegate,
};
use url::Url;
use winit::dpi::PhysicalSize;

use crate::{Bridge, RuntimeHandle};

/// How long the loop sleeps when nobody woke it: Servo asks for attention through the waker, a quit request
/// wakes it too, and this only bounds what a missed wake could cost.
const IDLE: Duration = Duration::from_millis(250);

#[derive(Clone, Default)]
struct Waker(Arc<(Mutex<bool>, Condvar)>);

impl Waker {
    /// Sleeps until Servo asks for attention or `limit` passes.
    fn sleep(&self, limit: Duration) {
        let (flag, signal) = &*self.0;
        let mut woken = flag.lock().unwrap_or_else(|e| e.into_inner());
        if !*woken {
            woken = signal
                .wait_timeout(woken, limit)
                .unwrap_or_else(|e| e.into_inner())
                .0;
        }
        *woken = false;
    }
}

impl EventLoopWaker for Waker {
    fn clone_box(&self) -> Box<dyn EventLoopWaker> {
        Box::new(self.clone())
    }

    fn wake(&self) {
        let (flag, signal) = &*self.0;
        *flag.lock().unwrap_or_else(|e| e.into_inner()) = true;
        signal.notify_one();
    }
}

struct Delegate {
    entry_url: Url,
    handle: RuntimeHandle,
    crashed: Cell<bool>,
}

impl WebViewDelegate for Delegate {
    fn load_web_resource(&self, _: WebView, load: WebResourceLoad) {
        crate::spikes::origin::web_resource(load);
    }

    fn request_navigation(&self, _: WebView, request: NavigationRequest) {
        let allowed = if self.entry_url.scheme() == "native" {
            request.url.scheme() == "native" && request.url.host_str() == Some("app")
        } else {
            request.url.origin() == self.entry_url.origin()
        };
        if allowed {
            request.allow();
        } else {
            request.deny();
        }
    }

    fn show_console_message(&self, _: WebView, level: ConsoleLogLevel, message: String) {
        eprintln!("Servo {level:?}: {message}");
    }

    fn notify_crashed(&self, _: WebView, reason: String, _: Option<String>) {
        eprintln!("Servo content crashed: {reason}");
        // An application whose document is gone has nothing left to do.
        if !self.crashed.replace(true) {
            self.handle.quit(1);
        }
    }
}

/// Runs the entry document of `bridge` in a hidden WebView until the application quits.
pub fn run_headless(bridge: &mut Bridge) -> Result<(), Box<dyn Error>> {
    let rendering = Rc::new(
        SoftwareRenderingContext::new(PhysicalSize::new(1, 1)).map_err(|error| {
            io::Error::other(format!(
                "no software rendering context ({error:?}): an application without a window needs OpenGL, \
                 and this machine has no driver that offers it"
            ))
        })?,
    );
    rendering
        .make_current()
        .map_err(|error| io::Error::other(format!("make_current: {error:?}")))?;
    let waker = Waker::default();
    let handle = bridge.handle();
    {
        let waker = waker.clone();
        handle.windowless(move || waker.wake());
    }
    let servo = ServoBuilder::default()
        .opts(Opts {
            multiprocess: false,
            temporary_storage: true,
            ..Opts::default()
        })
        .preferences(crate::spikes::origin::preferences())
        .protocol_registry(bridge.take_registry()?)
        .event_loop_waker(Box::new(waker.clone()))
        .build();
    servo.setup_logging();
    let delegate = Rc::new(Delegate {
        entry_url: bridge.entry_url.clone(),
        handle: handle.clone(),
        crashed: Cell::new(false),
    });
    let webview = WebViewBuilder::new(&servo, rendering.clone())
        .url(bridge.entry_url.clone())
        .delegate(delegate)
        .build();
    // The transport tells documents apart by the webview: this is window 1 of a process without a window.
    let ids = bridge.windows();
    ids.bind(webview.id(), ids.allocate());
    webview.hide();
    while !handle.quit_requested() {
        servo.spin_event_loop();
        if handle.quit_requested() {
            break;
        }
        waker.sleep(IDLE);
    }
    drop(webview);
    drop(servo);
    Ok(())
}
