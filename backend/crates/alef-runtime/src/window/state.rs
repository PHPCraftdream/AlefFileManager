// SPDX-License-Identifier: MPL-2.0
// Event-loop integration follows Servo 0.6's MPL-2.0 winit example.
use std::cell::Cell;
use std::io;
use std::rc::Rc;
use std::time::{Duration, Instant};

use super::resize_wait::{self, WakeGeneration};
use crate::ui::{event_json, event_script, WINDOW_STATE_EVENT};
use crate::{WindowAction, WindowState};
use serde_json::Value;
use servo::{DevicePoint, Modifiers, RenderingContext, Servo, WebView, WindowRenderingContext};
use winit::window::{CursorIcon, ResizeDirection, Window};

// The engine and GL context must be dropped before their window.
pub(super) struct State {
    pub(super) webview: WebView,
    pub(super) servo: Servo,
    pub(super) rendering: Rc<WindowRenderingContext>,
    pub(super) window: Rc<Window>,
    pub(super) animating: Rc<Cell<bool>>,
    pub(super) events: crate::bridge::EventBus,
    pub(super) window_id: u64,
    pub(super) cursor: DevicePoint,
    pub(super) modifiers: Modifiers,
    pub(super) composing: bool,
    pub(super) next_frame: Instant,
    pub(super) primary_pressed: bool,
    pub(super) snapshot: WindowState,
    pub(super) snapshot_dirty: bool,
    pub(super) published_revision: Option<u32>,
    pub(super) state_event_in_flight: Rc<Cell<bool>>,
    pub(super) page_ready: Rc<Cell<bool>>,
    pub(super) resize_hover: Option<ResizeDirection>,
    pub(super) frame_ready: Rc<Cell<bool>>,
    // BEGIN M0.2 multiwindow spike: present counter read by the spike oracle.
    pub(super) presents: Rc<Cell<u32>>,
    // END M0.2 multiwindow spike
    pub(super) resize_pending: bool,
    pub(super) native_resize_active: bool,
    pub(super) wake_gen: WakeGeneration,
}

const RESIZE_WAIT: Duration = Duration::from_millis(100);

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

    pub(super) fn trace_present(&self, path: &str, waited: Option<Duration>, timed_out: bool) {
        #[cfg(feature = "spike-integration")]
        crate::spikes::integration::note_present(timed_out);
        // BEGIN M0.2 multiwindow spike: present counter read by the spike oracle.
        self.presents.set(self.presents.get() + 1);
        // END M0.2 multiwindow spike
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

    pub(super) fn refresh_snapshot(&mut self) -> io::Result<()> {
        if !self.snapshot_dirty {
            return Ok(());
        }
        self.snapshot_dirty = false;
        let mut next = capture_window_state(&self.window);
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

    pub(super) fn publish_snapshot(&mut self) -> io::Result<()> {
        self.refresh_snapshot()?;
        if !self.page_ready.get()
            || self.state_event_in_flight.get()
            || self.published_revision == Some(self.snapshot.revision)
        {
            return Ok(());
        }
        let json = event_json(WINDOW_STATE_EVENT, &self.snapshot)?;
        self.events.publish(Some(self.window_id), &json);
        self.published_revision = Some(self.snapshot.revision);
        self.state_event_in_flight.set(true);
        let in_flight = self.state_event_in_flight.clone();
        self.webview
            .evaluate_javascript(event_script(&json), move |result| {
                in_flight.set(false);
                if let Err(error) = result {
                    eprintln!("Window state event failed: {error:?}");
                }
            });
        Ok(())
    }

    pub(super) fn window_action(&mut self, action: WindowAction) -> io::Result<Value> {
        match action {
            WindowAction::GetState => {
                self.snapshot_dirty = true;
                self.refresh_snapshot()?;
                return serde_json::to_value(&self.snapshot).map_err(io::Error::other);
            }
            WindowAction::Minimize => self.window.set_minimized(true),
            WindowAction::Maximize => self.window.set_maximized(true),
            WindowAction::Restore => {
                if self.window.is_minimized() == Some(true) {
                    self.window.set_minimized(false);
                }
                if self.window.is_maximized() {
                    self.window.set_maximized(false);
                }
            }
            WindowAction::ToggleMaximize => self.window.set_maximized(!self.window.is_maximized()),
            WindowAction::SetDecorations { enabled } => self.window.set_decorations(enabled),
            WindowAction::SetResizable { enabled } => self.window.set_resizable(enabled),
            WindowAction::StartResize { .. } if !self.window.is_resizable() => {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "Window resizing is disabled",
                ));
            }
            WindowAction::StartDrag | WindowAction::StartResize { .. } if !self.primary_pressed => {
                return Err(io::Error::new(
                    io::ErrorKind::PermissionDenied,
                    "Drag requires a pressed primary mouse button",
                ));
            }
            WindowAction::StartDrag => {
                self.window.drag_window().map_err(io::Error::other)?;
                self.primary_pressed = false;
            }
            WindowAction::StartResize { edge } => {
                let edge: ResizeDirection = edge.into();
                self.window
                    .drag_resize_window(edge)
                    .map_err(io::Error::other)?;
                self.native_resize_active = true;
                self.primary_pressed = false;
            }
            WindowAction::Close => {}
        }
        self.snapshot_dirty = true;
        self.update_resize_cursor();
        Ok(Value::Null)
    }
}

pub(super) fn capture_window_state(window: &Window) -> WindowState {
    let size = window.inner_size();
    let position = window.outer_position().ok();
    WindowState {
        revision: 0,
        title: window.title(),
        width: size.width,
        height: size.height,
        x: position.map(|position| position.x),
        y: position.map(|position| position.y),
        scale_factor: window.scale_factor(),
        focused: window.has_focus(),
        maximized: window.is_maximized(),
        minimized: window.is_minimized(),
        visible: window.is_visible(),
        decorated: window.is_decorated(),
        resizable: window.is_resizable(),
        fullscreen: window.fullscreen().is_some(),
        supports_drag_resize: super::platform::SUPPORTS_NATIVE_RESIZE,
    }
}
