// SPDX-License-Identifier: MPL-2.0
// Event-loop integration follows Servo 0.6's MPL-2.0 winit example.
use std::cell::Cell;
use std::rc::{Rc, Weak};

use super::resize_wait::{self, WakeGeneration};
use crate::ui::Wake;
use servo::{
    ConsoleLogLevel, EventLoopWaker, LoadStatus, NavigationRequest, WebResourceLoad, WebView,
    WebViewDelegate,
};
use url::Url;
use winit::event_loop::EventLoopProxy;
use winit::window::Window;

#[derive(Clone)]
pub(super) struct Waker(pub(super) EventLoopProxy<Wake>, pub(super) WakeGeneration);

impl EventLoopWaker for Waker {
    fn clone_box(&self) -> Box<dyn EventLoopWaker> {
        Box::new(self.clone())
    }
    fn wake(&self) {
        resize_wait::bump_generation(&self.1);
        let _ = self.0.send_event(Wake);
    }
}

pub(super) struct Delegate {
    pub(super) window: Weak<Window>,
    pub(super) entry_url: Url,
    pub(super) title: String,
    pub(super) animating: Rc<Cell<bool>>,
    pub(super) page_ready: Rc<Cell<bool>>,
    pub(super) frame_ready: Rc<Cell<bool>>,
}

impl WebViewDelegate for Delegate {
    fn notify_new_frame_ready(&self, _: WebView) {
        self.frame_ready.set(true);
        if let Some(window) = self.window.upgrade() {
            window.request_redraw();
        }
    }

    fn notify_animating_changed(&self, _: WebView, animating: bool) {
        self.animating.set(animating);
        if let Some(window) = self.window.upgrade() {
            window.request_redraw();
        }
    }

    fn notify_load_status_changed(&self, _: WebView, status: LoadStatus) {
        // Sessions follow documents, not load statuses: Servo reports no `Started` for the first
        // load, and the embedder sees the status after the page may already be running.
        // `runtime.hello` creates the session of the calling document (see `SessionManager`).
        self.page_ready.set(status == LoadStatus::Complete);
        if status == LoadStatus::Complete {
            if let Some(window) = self.window.upgrade() {
                window.request_redraw();
            }
        }
    }

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
        if let Some(window) = self.window.upgrade() {
            window.set_title(&format!("{} — renderer crashed", self.title));
        }
    }
}
