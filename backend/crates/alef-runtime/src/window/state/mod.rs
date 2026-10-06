// SPDX-License-Identifier: MPL-2.0
// Event-loop integration follows Servo 0.6's MPL-2.0 winit example.
pub(super) mod resize_wait;

use std::cell::Cell;
use std::io;
use std::rc::Rc;
use std::time::{Duration, Instant};

use super::host::events;
use crate::ui::{event_json, WINDOW_STATE_EVENT};
use alef_core::{registry::window::WindowInfo, SessionId};
use resize_wait::WakeGeneration;
use servo::{
    DeviceIntRect, DeviceIntSize, DevicePoint, Modifiers, RenderingContext, Servo, WebView,
    WindowRenderingContext,
};
use winit::window::{CursorIcon, ResizeDirection, Window};

/// A close request the document has been asked about (`window.close-requested`).
pub(super) struct PendingClose {
    pub(super) id: u64,
    pub(super) deadline: Instant,
}

// The engine and GL context must be dropped before their window.
pub(super) struct State {
    pub(super) webview: WebView,
    pub(super) servo: Servo,
    pub(super) rendering: Rc<WindowRenderingContext>,
    pub(super) window: Rc<Window>,
    pub(super) animating: Rc<Cell<bool>>,
    pub(super) events: crate::bridge::EventBus,
    pub(super) window_id: u64,
    pub(super) label: String,
    pub(super) cursor: DevicePoint,
    pub(super) modifiers: Modifiers,
    pub(super) composing: bool,
    pub(super) next_frame: Instant,
    pub(super) primary_pressed: bool,
    pub(super) snapshot: WindowInfo,
    pub(super) snapshot_dirty: bool,
    /// What the documents were last told; the next events are the difference to it.
    pub(super) published: Option<WindowInfo>,
    pub(super) page_ready: Rc<Cell<bool>>,
    pub(super) resize_hover: Option<ResizeDirection>,
    pub(super) frame_ready: Rc<Cell<bool>>,
    pub(super) content_frame: Rc<Cell<bool>>,
    /// The window has been shown (it starts hidden, see `try_reveal`).
    pub(super) revealed: bool,
    pub(super) created: Instant,
    pub(super) ready_since: Option<Instant>,
    pub(super) resize_pending: bool,
    pub(super) native_resize_active: bool,
    pub(super) wake_gen: WakeGeneration,
    pub(super) always_on_top: bool,
    /// Limits of the inner size, logical pixels: the system holds the user to them, the runtime
    /// holds `window.setSize` to them (Windows ignores them for programmatic resizing).
    pub(super) min_size: Option<(f64, f64)>,
    pub(super) max_size: Option<(f64, f64)>,
    /// The session whose document answers close requests of this window.
    pub(super) intercept: Option<SessionId>,
    pub(super) pending_close: Option<PendingClose>,
}

const RESIZE_WAIT: Duration = Duration::from_millis(100);
/// How often a hidden window checks whether it may be shown.
pub(super) const REVEAL_POLL: Duration = Duration::from_millis(50);
/// A page whose frames stay blank (or that announces none) is shown this long after it loaded.
const REVEAL_SETTLE: Duration = Duration::from_millis(600);
/// A page that never finishes loading must not leave the application invisible.
const REVEAL_LIMIT: Duration = Duration::from_secs(10);

impl State {
    pub(super) fn synchronize_viewport(&mut self) -> io::Result<bool> {
        let size = self.window.inner_size();
        if size.width == 0 || size.height == 0 {
            return Ok(false);
        }
        if self.rendering.size() != size {
            self.frame_ready.set(false);
            self.resize_pending = true;
            self.webview.resize(size);
            self.snapshot_dirty = true;
            if self.rendering.size() != size {
                return Err(io::Error::other(
                    "Servo surface does not match native client size",
                ));
            }
        }
        Ok(true)
    }

    pub(super) fn redraw(&mut self) -> io::Result<()> {
        if !self.synchronize_viewport()? {
            return Ok(());
        }
        self.servo.spin_event_loop();
        if self.resize_pending && !self.frame_ready.get() {
            return Ok(());
        }
        self.frame_ready.set(false);
        self.resize_pending = false;
        self.webview.paint();
        self.trace_present("redraw", None, false);
        self.rendering.present();
        self.next_frame = Instant::now() + Duration::from_millis(16);
        Ok(())
    }

    /// Shows the window the first time there is something to show. Before that it is an unpainted
    /// rectangle - white, with a black strip where it grew to its requested size - and the page
    /// itself is blank: the load finishes before the application has drawn anything. So once the page
    /// has loaded, every frame it announces is painted off screen and looked at; the first one that is
    /// not a single colour is presented and the window is shown. A page that stays blank is shown
    /// `REVEAL_SETTLE` after it loaded, one that never loads after `REVEAL_LIMIT`.
    pub(super) fn try_reveal(&mut self) -> io::Result<()> {
        if self.revealed {
            return Ok(());
        }
        if self.page_ready.get() && self.ready_since.is_none() {
            self.ready_since = Some(Instant::now());
        }
        let overdue = self.created.elapsed() >= REVEAL_LIMIT
            || self
                .ready_since
                .is_some_and(|since| since.elapsed() >= REVEAL_SETTLE);
        if overdue {
            self.redraw()?;
        } else if self.page_ready.get() && self.content_frame.replace(false) {
            if !self.synchronize_viewport()? {
                return Ok(());
            }
            self.servo.spin_event_loop();
            self.frame_ready.set(false);
            self.webview.paint();
            if !self.painted_frame_has_content() {
                return Ok(());
            }
            self.trace_present("reveal", None, false);
            self.rendering.present();
        } else {
            return Ok(());
        }
        self.window.set_visible(true);
        self.window.focus_window();
        self.revealed = true;
        self.snapshot_dirty = true;
        Ok(())
    }

    /// Whether the frame just painted is more than one colour (read back before it is presented).
    fn painted_frame_has_content(&self) -> bool {
        let size = self.rendering.size();
        let rectangle = DeviceIntRect::from_size(DeviceIntSize::new(
            i32::try_from(size.width).unwrap_or(i32::MAX),
            i32::try_from(size.height).unwrap_or(i32::MAX),
        ));
        self.rendering
            .read_to_image(rectangle)
            .is_some_and(|image| {
                let mut pixels = image.pixels();
                pixels
                    .next()
                    .is_some_and(|first| pixels.any(|pixel| pixel != first))
            })
    }

    pub(super) fn trace_present(&self, path: &str, waited: Option<Duration>, timed_out: bool) {
        #[cfg(feature = "spike-integration")]
        crate::spikes::integration::note_present(timed_out);
        if resize_wait::resize_trace_enabled() {
            let surface = self.rendering.size();
            let window = self.window.inner_size();
            eprintln!(
                "resize trace: {path} surface {}x{} window {}x{} waited={:?} timed_out={timed_out}",
                surface.width, surface.height, window.width, window.height, waited
            );
        }
    }

    // Bounded synchronous wait for a frame at the new size, then present it.
    pub(super) fn wait_for_resize_frame(&mut self) {
        let started = Instant::now();
        let deadline = started + RESIZE_WAIT;
        let mut timed_out = false;
        loop {
            // Read before spinning so a wake during the spin is not lost.
            let seen = *self.wake_gen.0.lock().unwrap();
            self.servo.spin_event_loop();
            if self.frame_ready.get() {
                break;
            }
            if !resize_wait::wait_for_generation(&self.wake_gen, seen, deadline) {
                timed_out = true;
                break;
            }
        }
        if timed_out {
            let size = self.window.inner_size();
            eprintln!(
                "resize: timed out waiting for content at {}x{}",
                size.width, size.height
            );
        }
        self.frame_ready.set(false);
        self.resize_pending = false;
        self.webview.paint();
        self.trace_present("resize", Some(started.elapsed()), timed_out);
        self.rendering.present();
    }

    pub(super) fn resize_edge(&self) -> Option<ResizeDirection> {
        if !self.window.is_resizable()
            || self.window.is_decorated()
            || self.window.is_maximized()
            || !self.snapshot.supports_drag_resize
        {
            return None;
        }
        super::input::frame::resize_hit(
            self.cursor,
            self.window.inner_size(),
            self.window.scale_factor(),
        )
    }

    pub(super) fn update_resize_cursor(&mut self) {
        if self.native_resize_active {
            return;
        }
        let edge = self.resize_edge();
        if edge == self.resize_hover {
            return;
        }
        self.resize_hover = edge;
        self.window
            .set_cursor(edge.map(CursorIcon::from).unwrap_or(CursorIcon::Default));
    }

    /// The window as it is now; sizes and positions in logical pixels.
    pub(super) fn capture(&self) -> WindowInfo {
        capture_info(&self.label, &self.window, &self.webview, self.always_on_top)
    }

    /// Re-reads the window when something may have changed.
    pub(super) fn refresh_snapshot(&mut self) -> io::Result<()> {
        if !self.snapshot_dirty {
            return Ok(());
        }
        self.snapshot_dirty = false;
        let mut next = self.capture();
        next.revision = self.snapshot.revision;
        if next != self.snapshot {
            next.revision = next
                .revision
                .checked_add(1)
                .ok_or_else(|| io::Error::other("Window state revision exhausted"))?;
            self.snapshot = next;
        }
        Ok(())
    }

    /// The window as it is now, read fresh.
    pub(super) fn fresh_snapshot(&mut self) -> io::Result<WindowInfo> {
        self.snapshot_dirty = true;
        self.refresh_snapshot()?;
        Ok(self.snapshot.clone())
    }

    /// Tells the documents what changed: the snapshot to the window's own document, and the
    /// `window.moved`/`resized`/`focus`/`blur` events to every document.
    pub(super) fn publish_snapshot(&mut self) -> io::Result<()> {
        self.refresh_snapshot()?;
        let current = self.snapshot.revision;
        if !self.page_ready.get()
            || self
                .published
                .as_ref()
                .is_some_and(|p| p.revision == current)
        {
            return Ok(());
        }
        if let Some(previous) = &self.published {
            for (name, payload) in events::changes(previous, &self.snapshot) {
                self.events.publish(None, &event_json(name, &payload)?);
            }
        }
        let json = event_json(WINDOW_STATE_EVENT, &self.snapshot)?;
        self.events.publish(Some(self.window_id), &json);
        self.published = Some(self.snapshot.clone());
        Ok(())
    }
}

/// The window as it is now; sizes and positions in logical pixels.
pub(super) fn capture_info(
    label: &str,
    window: &Window,
    webview: &WebView,
    always_on_top: bool,
) -> WindowInfo {
    let scale = window.scale_factor();
    let size = window.inner_size();
    let position = window.outer_position().ok();
    WindowInfo {
        label: label.to_owned(),
        revision: 0,
        title: window.title(),
        width: f64::from(size.width) / scale,
        height: f64::from(size.height) / scale,
        x: position.map(|position| f64::from(position.x) / scale),
        y: position.map(|position| f64::from(position.y) / scale),
        scale_factor: scale,
        focused: window.has_focus(),
        maximized: window.is_maximized(),
        minimized: window.is_minimized(),
        visible: window.is_visible(),
        decorated: window.is_decorated(),
        resizable: window.is_resizable(),
        fullscreen: window.fullscreen().is_some(),
        always_on_top,
        zoom: f64::from(webview.page_zoom()),
        supports_drag_resize: super::platform::SUPPORTS_NATIVE_RESIZE,
    }
}
