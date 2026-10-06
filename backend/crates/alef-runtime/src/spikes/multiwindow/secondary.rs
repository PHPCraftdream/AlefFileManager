// SPDX-License-Identifier: MIT OR Apache-2.0
// The secondary window: own rendering context and webview on the same Servo instance.
use std::cell::Cell;
use std::io;
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

use euclid::Scale;
use servo::{
    Code, ConsoleLogLevel, DevicePoint, InputEvent, Key, KeyState, KeyboardEvent, LoadStatus,
    Modifiers, MouseButton, MouseButtonAction, MouseButtonEvent, MouseLeftViewportEvent,
    MouseMoveEvent, NamedKey, NavigationRequest, RenderingContext, Servo, WebView, WebViewBuilder,
    WebViewDelegate, WheelDelta, WheelEvent, WheelMode, WindowRenderingContext,
};
use url::Url;
use winit::dpi::LogicalSize;
use winit::event::{ElementState, MouseScrollDelta, WindowEvent};
use winit::event_loop::ActiveEventLoop;
use winit::keyboard::{Key as WinitKey, KeyLocation, PhysicalKey};
use winit::raw_window_handle::{HasDisplayHandle, HasWindowHandle};
use winit::window::Window;

use crate::window::resize_wait::{wait_for_generation, WakeGeneration};

// Bounded synchronous wait for a frame at the new size (mirrors window/state.rs).
pub(in crate::spikes::multiwindow) const RESIZE_WAIT: Duration = Duration::from_millis(100);

pub(in crate::spikes::multiwindow) struct Secondary {
    pub servo: Servo,
    pub webview: WebView,
    pub rendering: Rc<WindowRenderingContext>,
    pub window: Rc<Window>,
    pub animating: Rc<Cell<bool>>,
    pub page_ready: Rc<Cell<bool>>,
    pub frame_ready: Rc<Cell<bool>>,
    /// Servo content-process crash notifications seen for this window.
    pub crashed: Rc<Cell<u32>>,
    pub wake_gen: WakeGeneration,
    cursor: DevicePoint,
    modifiers: Modifiers,
    pub next_frame: Instant,
    resize_pending: bool,
    trace: bool,
    /// Presents performed (paint+present), resize-wait ones included.
    pub presents: u32,
    /// Resize-wait timeouts inside the 100 ms budget.
    pub timeouts: u32,
    /// Surface size of the last present, physical pixels.
    last_present_size: (u32, u32),
}

impl Secondary {
    pub(in crate::spikes::multiwindow) fn create(
        servo: &Servo,
        event_loop: &ActiveEventLoop,
        url: Url,
        wake_gen: WakeGeneration,
        trace: bool,
    ) -> Result<Self, String> {
        let attributes = Window::default_attributes()
            .with_title("M0.2 multiwindow spike")
            .with_inner_size(LogicalSize::new(900.0, 650.0))
            .with_decorations(false)
            .with_resizable(true);
        let window = Rc::new(
            event_loop
                .create_window(attributes)
                .map_err(|error| format!("winit window creation failed: {error}"))?,
        );
        window.set_ime_allowed(true);
        let rendering = Rc::new(
            WindowRenderingContext::new(
                event_loop
                    .display_handle()
                    .map_err(|error| error.to_string())?,
                window.window_handle().map_err(|error| error.to_string())?,
                window.inner_size(),
            )
            .map_err(|error| format!("secondary Servo graphics context failed: {error:?}"))?,
        );
        rendering
            .make_current()
            .map_err(|error| format!("secondary graphics context activation failed: {error:?}"))?;
        let (animating, page_ready, frame_ready) = (
            Rc::new(Cell::new(false)),
            Rc::new(Cell::new(false)),
            Rc::new(Cell::new(false)),
        );
        let crashed = Rc::new(Cell::new(0));
        let delegate = Rc::new(SpikeDelegate {
            window: Rc::downgrade(&window),
            animating: animating.clone(),
            page_ready: page_ready.clone(),
            frame_ready: frame_ready.clone(),
            crashed: crashed.clone(),
        });
        let webview = WebViewBuilder::new(servo, rendering.clone())
            .url(url)
            .hidpi_scale_factor(Scale::new(window.scale_factor() as f32))
            .delegate(delegate)
            .build();
        webview.show();
        webview.focus();
        servo.spin_event_loop();
        Ok(Self {
            servo: servo.clone(),
            webview,
            rendering,
            window,
            animating,
            page_ready,
            frame_ready,
            crashed,
            wake_gen,
            cursor: DevicePoint::zero(),
            modifiers: Modifiers::empty(),
            next_frame: Instant::now() + Duration::from_millis(16),
            resize_pending: false,
            trace,
            presents: 0,
            timeouts: 0,
            last_present_size: (0, 0),
        })
    }

    /// Surface size of the last present (physical pixels).
    pub(in crate::spikes::multiwindow) fn last_present_size(&self) -> (u32, u32) {
        self.last_present_size
    }

    /// Mirrors window/app.rs `window_event` for the secondary window (no snapshot, no IME).
    pub(in crate::spikes::multiwindow) fn on_event(
        &mut self,
        event: &WindowEvent,
    ) -> io::Result<()> {
        match event {
            WindowEvent::RedrawRequested => self.redraw()?,
            WindowEvent::Resized(_) => {
                if self.synchronize_viewport()? && self.resize_pending {
                    self.wait_for_resize_frame();
                }
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                self.webview
                    .set_hidpi_scale_factor(Scale::new(*scale_factor as f32));
                self.synchronize_viewport()?;
            }
            WindowEvent::Focused(focused) => {
                if *focused {
                    self.webview.focus();
                } else {
                    self.webview.blur();
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                self.cursor = DevicePoint::new(position.x as f32, position.y as f32);
                self.webview
                    .notify_input_event(InputEvent::MouseMove(MouseMoveEvent::new(
                        self.cursor.into(),
                    )));
            }
            WindowEvent::CursorLeft { .. } => {
                self.webview
                    .notify_input_event(InputEvent::MouseLeftViewport(
                        MouseLeftViewportEvent::default(),
                    ));
            }
            WindowEvent::MouseInput {
                state: pressed,
                button,
                ..
            } => {
                let button = match button {
                    winit::event::MouseButton::Left => MouseButton::Primary,
                    winit::event::MouseButton::Right => MouseButton::Secondary,
                    winit::event::MouseButton::Middle => MouseButton::Auxiliary,
                    winit::event::MouseButton::Back => MouseButton::Back,
                    winit::event::MouseButton::Forward => MouseButton::Forward,
                    winit::event::MouseButton::Other(value) => MouseButton::Other(*value),
                };
                let action = if *pressed == ElementState::Pressed {
                    MouseButtonAction::Down
                } else {
                    MouseButtonAction::Up
                };
                self.webview
                    .notify_input_event(InputEvent::MouseButton(MouseButtonEvent::new(
                        action,
                        button,
                        self.cursor.into(),
                    )));
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let delta = wheel_delta(delta, self.window.scale_factor());
                self.webview
                    .notify_input_event(InputEvent::Wheel(WheelEvent::new(
                        delta,
                        self.cursor.into(),
                    )));
            }
            WindowEvent::ModifiersChanged(modifiers) => {
                let native = modifiers.state();
                self.modifiers = Modifiers::empty();
                self.modifiers.set(Modifiers::SHIFT, native.shift_key());
                self.modifiers.set(Modifiers::CONTROL, native.control_key());
                self.modifiers.set(Modifiers::ALT, native.alt_key());
                self.modifiers.set(Modifiers::META, native.super_key());
            }
            WindowEvent::KeyboardInput { event, .. } => self.on_keyboard(event),
            _ => {}
        }
        self.servo.spin_event_loop();
        Ok(())
    }

    fn on_keyboard(&mut self, event: &winit::event::KeyEvent) {
        let key = match &event.logical_key {
            WinitKey::Character(text) => Key::Character(text.to_string()),
            WinitKey::Named(named) => format!("{named:?}")
                .parse()
                .unwrap_or(Key::Named(NamedKey::Unidentified)),
            _ => Key::Named(NamedKey::Unidentified),
        };
        let code = match event.physical_key {
            PhysicalKey::Code(code) => format!("{code:?}").parse().unwrap_or(Code::Unidentified),
            _ => Code::Unidentified,
        };
        let location = match event.location {
            KeyLocation::Standard => servo::Location::Standard,
            KeyLocation::Left => servo::Location::Left,
            KeyLocation::Right => servo::Location::Right,
            KeyLocation::Numpad => servo::Location::Numpad,
        };
        let pressed = if event.state == ElementState::Pressed {
            KeyState::Down
        } else {
            KeyState::Up
        };
        let keyboard = KeyboardEvent::new_without_event(
            pressed,
            key,
            code,
            location,
            self.modifiers,
            event.repeat,
            false,
        );
        self.webview
            .notify_input_event(InputEvent::Keyboard(keyboard));
    }

    // Mirror of State::synchronize_viewport without snapshot bookkeeping.
    fn synchronize_viewport(&mut self) -> io::Result<bool> {
        let size = self.window.inner_size();
        if size.width == 0 || size.height == 0 {
            return Ok(false);
        }
        if self.rendering.size() != size {
            self.frame_ready.set(false);
            self.resize_pending = true;
            self.webview.resize(size);
            if self.rendering.size() != size {
                return Err(io::Error::other(
                    "Servo surface does not match native client size",
                ));
            }
        }
        Ok(true)
    }

    fn redraw(&mut self) -> io::Result<()> {
        if !self.synchronize_viewport()? {
            return Ok(());
        }
        self.servo.spin_event_loop();
        if self.resize_pending && !self.frame_ready.get() {
            return Ok(());
        }
        self.frame_ready.set(false);
        self.resize_pending = false;
        self.paint_and_present("redraw", None, false);
        self.next_frame = Instant::now() + Duration::from_millis(16);
        Ok(())
    }

    // Mirror of State::wait_for_resize_frame: bounded synchronous wait, then paint+present.
    fn wait_for_resize_frame(&mut self) {
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
            if !wait_for_generation(&self.wake_gen, seen, deadline) {
                timed_out = true;
                break;
            }
        }
        if timed_out {
            let size = self.window.inner_size();
            eprintln!(
                "MW w2 resize: timed out waiting for content at {}x{}",
                size.width, size.height
            );
        }
        self.paint_and_present("resize", Some(started.elapsed()), timed_out);
    }

    fn paint_and_present(&mut self, path: &str, waited: Option<Duration>, timed_out: bool) {
        self.frame_ready.set(false);
        self.resize_pending = false;
        self.webview.paint();
        self.presents += 1;
        if timed_out {
            self.timeouts += 1;
        }
        let surface = self.rendering.size();
        self.last_present_size = (surface.width, surface.height);
        if self.trace {
            eprintln!(
                "MW w2 resize trace: {path} surface {}x{} waited={waited:?} timed_out={timed_out}",
                surface.width, surface.height
            );
        }
        self.rendering.present();
    }
}

struct SpikeDelegate {
    window: Weak<Window>,
    animating: Rc<Cell<bool>>,
    page_ready: Rc<Cell<bool>>,
    frame_ready: Rc<Cell<bool>>,
    crashed: Rc<Cell<u32>>,
}

impl WebViewDelegate for SpikeDelegate {
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
        self.page_ready.set(status == LoadStatus::Complete);
        if status == LoadStatus::Complete {
            if let Some(window) = self.window.upgrade() {
                window.request_redraw();
            }
        }
    }

    fn request_navigation(&self, _: WebView, request: NavigationRequest) {
        // Same guard as the app delegate: only the app's native origin.
        if request.url.scheme() == "native" && request.url.host_str() == Some("app") {
            request.allow();
        } else {
            request.deny();
        }
    }

    fn show_console_message(&self, _: WebView, level: ConsoleLogLevel, message: String) {
        eprintln!("MW w2 Servo {level:?}: {message}");
    }

    fn notify_crashed(&self, _: WebView, reason: String, _: Option<String>) {
        self.crashed.set(self.crashed.get() + 1);
        eprintln!("MW w2 Servo content crashed: {reason}");
    }
}

// Simplified window/input/wheel.rs policy (the spike page never scrolls).
fn wheel_delta(delta: &MouseScrollDelta, scale: f64) -> WheelDelta {
    let (x, y) = match delta {
        MouseScrollDelta::PixelDelta(position) => (position.x, position.y),
        MouseScrollDelta::LineDelta(x, y) => (*x as f64 * 76.0, *y as f64 * 76.0 * scale),
    };
    WheelDelta {
        x,
        y,
        z: 0.0,
        mode: WheelMode::DeltaPixel,
    }
}
