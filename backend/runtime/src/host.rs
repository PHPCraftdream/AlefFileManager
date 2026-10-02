// SPDX-License-Identifier: MPL-2.0
// Event-loop integration follows Servo 0.6's MPL-2.0 winit example.
use std::cell::{Cell, RefCell};
use std::io;
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

use crate::Bridge;
use euclid::Scale;
use servo::protocol_handler::ProtocolRegistry;
use servo::{
    Code, CompositionEvent, CompositionState, ConsoleLogLevel, DevicePoint, EventLoopWaker,
    ImeEvent, InputEvent, Key, KeyState, KeyboardEvent, Location, Modifiers, MouseButton,
    MouseButtonAction, MouseButtonEvent, MouseLeftViewportEvent, MouseMoveEvent, NamedKey,
    NavigationRequest, Opts, RenderingContext, Servo, ServoBuilder, WebView, WebViewBuilder,
    WebViewDelegate, WheelDelta, WheelEvent, WheelMode, WindowRenderingContext,
};
use url::Url;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, Ime, MouseScrollDelta, StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key as WinitKey, KeyLocation, PhysicalKey};
use winit::raw_window_handle::{HasDisplayHandle, HasWindowHandle};
use winit::window::{Icon, Window, WindowId};

#[derive(Clone, Debug)]
struct Wake;

#[derive(Clone)]
struct Waker(EventLoopProxy<Wake>);

impl EventLoopWaker for Waker {
    fn clone_box(&self) -> Box<dyn EventLoopWaker> {
        Box::new(self.clone())
    }
    fn wake(&self) {
        let _ = self.0.send_event(Wake);
    }
}

struct Delegate {
    window: Weak<Window>,
    entry_url: Url,
    title: String,
    animating: Rc<Cell<bool>>,
}

impl WebViewDelegate for Delegate {
    fn notify_new_frame_ready(&self, _: WebView) {
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

// The engine and GL context must be dropped before their window.
struct State {
    webview: WebView,
    servo: Servo,
    rendering: Rc<WindowRenderingContext>,
    window: Rc<Window>,
    animating: Rc<Cell<bool>>,
    cursor: DevicePoint,
    modifiers: Modifiers,
    composing: bool,
    next_frame: Instant,
}

pub struct WindowOptions {
    pub title: String,
    pub width: f64,
    pub height: f64,
    pub icon_png: Vec<u8>,
}

struct App {
    url: Url,
    options: WindowOptions,
    registry: RefCell<Option<ProtocolRegistry>>,
    waker: Waker,
    state: Option<State>,
    error: Option<io::Error>,
}

impl App {
    fn initialize(
        &self,
        event_loop: &ActiveEventLoop,
    ) -> Result<State, Box<dyn std::error::Error>> {
        let icon = image::load_from_memory(&self.options.icon_png)?.to_rgba8();
        let (width, height) = icon.dimensions();
        let icon = Icon::from_rgba(icon.into_raw(), width, height)?;
        let window = Rc::new(
            event_loop.create_window(
                Window::default_attributes()
                    .with_title(&self.options.title)
                    .with_inner_size(winit::dpi::LogicalSize::new(
                        self.options.width,
                        self.options.height,
                    ))
                    .with_window_icon(Some(icon)),
            )?,
        );
        window.set_ime_allowed(true);
        let rendering = Rc::new(
            WindowRenderingContext::new(
                event_loop.display_handle()?,
                window.window_handle()?,
                window.inner_size(),
            )
            .map_err(|error| {
                io::Error::other(format!("Cannot create Servo graphics context: {error:?}"))
            })?,
        );
        rendering.make_current().map_err(|error| {
            io::Error::other(format!("Cannot activate Servo graphics context: {error:?}"))
        })?;
        let servo = ServoBuilder::default()
            .opts(Opts {
                multiprocess: false,
                temporary_storage: true,
                ..Opts::default()
            })
            .protocol_registry(
                self.registry
                    .borrow_mut()
                    .take()
                    .ok_or_else(|| io::Error::other("Protocol registry already consumed"))?,
            )
            .event_loop_waker(Box::new(self.waker.clone()))
            .build();
        servo.setup_logging();
        let animating = Rc::new(Cell::new(false));
        let delegate = Rc::new(Delegate {
            window: Rc::downgrade(&window),
            entry_url: self.url.clone(),
            title: self.options.title.clone(),
            animating: animating.clone(),
        });
        let webview = WebViewBuilder::new(&servo, rendering.clone())
            .url(self.url.clone())
            .hidpi_scale_factor(Scale::new(window.scale_factor() as f32))
            .delegate(delegate)
            .build();
        webview.show();
        webview.focus();
        servo.spin_event_loop();
        Ok(State {
            webview,
            servo,
            rendering,
            window,
            animating,
            cursor: DevicePoint::zero(),
            modifiers: Modifiers::empty(),
            composing: false,
            next_frame: Instant::now() + Duration::from_millis(16),
        })
    }
}

impl ApplicationHandler<Wake> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.state.is_some() {
            return;
        }
        match self.initialize(event_loop) {
            Ok(state) => self.state = Some(state),
            Err(error) => {
                self.error = Some(io::Error::other(error.to_string()));
                event_loop.exit();
            }
        }
    }

    fn user_event(&mut self, _: &ActiveEventLoop, _: Wake) {
        if let Some(state) = &self.state {
            state.servo.spin_event_loop();
        }
    }

    fn new_events(&mut self, _: &ActiveEventLoop, cause: StartCause) {
        if matches!(cause, StartCause::ResumeTimeReached { .. }) {
            if let Some(state) = &self.state {
                if state.animating.get() {
                    state.window.request_redraw();
                }
            }
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(state) = &self.state {
            state.servo.spin_event_loop();
            if state.animating.get() {
                event_loop.set_control_flow(ControlFlow::WaitUntil(state.next_frame));
            } else {
                event_loop.set_control_flow(ControlFlow::Wait);
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        if matches!(event, WindowEvent::CloseRequested) {
            self.state.take();
            event_loop.exit();
            return;
        }
        let Some(state) = self.state.as_mut() else {
            return;
        };
        match event {
            WindowEvent::RedrawRequested => {
                state.webview.paint();
                state.rendering.present();
                state.next_frame = Instant::now() + Duration::from_millis(16);
            }
            WindowEvent::Resized(size) => {
                state.webview.resize(size);
                state.window.request_redraw();
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                state
                    .webview
                    .set_hidpi_scale_factor(Scale::new(scale_factor as f32));
                state.webview.resize(state.window.inner_size());
            }
            WindowEvent::Focused(focused) => {
                if focused {
                    state.webview.focus();
                } else {
                    state.webview.blur();
                }
            }
            WindowEvent::CursorMoved { position, .. } => {
                state.cursor = DevicePoint::new(position.x as f32, position.y as f32);
                state
                    .webview
                    .notify_input_event(InputEvent::MouseMove(MouseMoveEvent::new(
                        state.cursor.into(),
                    )));
            }
            WindowEvent::CursorLeft { .. } => {
                state
                    .webview
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
                    winit::event::MouseButton::Other(value) => MouseButton::Other(value),
                };
                let action = if pressed == ElementState::Pressed {
                    MouseButtonAction::Down
                } else {
                    MouseButtonAction::Up
                };
                state
                    .webview
                    .notify_input_event(InputEvent::MouseButton(MouseButtonEvent::new(
                        action,
                        button,
                        state.cursor.into(),
                    )));
            }
            WindowEvent::MouseWheel { delta, .. } => {
                let (x, y, mode) = match delta {
                    MouseScrollDelta::LineDelta(x, y) => {
                        (f64::from(x), f64::from(y), WheelMode::DeltaLine)
                    }
                    MouseScrollDelta::PixelDelta(position) => {
                        (position.x, position.y, WheelMode::DeltaPixel)
                    }
                };
                state
                    .webview
                    .notify_input_event(InputEvent::Wheel(WheelEvent::new(
                        WheelDelta { x, y, z: 0.0, mode },
                        state.cursor.into(),
                    )));
            }
            WindowEvent::ModifiersChanged(modifiers) => {
                let native = modifiers.state();
                state.modifiers = Modifiers::empty();
                state.modifiers.set(Modifiers::SHIFT, native.shift_key());
                state
                    .modifiers
                    .set(Modifiers::CONTROL, native.control_key());
                state.modifiers.set(Modifiers::ALT, native.alt_key());
                state.modifiers.set(Modifiers::META, native.super_key());
            }
            WindowEvent::KeyboardInput { event, .. } => {
                let key = match event.logical_key {
                    WinitKey::Character(text) => Key::Character(text.to_string()),
                    WinitKey::Named(named) => format!("{named:?}")
                        .parse()
                        .unwrap_or(Key::Named(NamedKey::Unidentified)),
                    _ => Key::Named(NamedKey::Unidentified),
                };
                let code = match event.physical_key {
                    PhysicalKey::Code(code) => {
                        format!("{code:?}").parse().unwrap_or(Code::Unidentified)
                    }
                    _ => Code::Unidentified,
                };
                let location = match event.location {
                    KeyLocation::Standard => Location::Standard,
                    KeyLocation::Left => Location::Left,
                    KeyLocation::Right => Location::Right,
                    KeyLocation::Numpad => Location::Numpad,
                };
                let pressed = if event.state == ElementState::Pressed {
                    KeyState::Down
                } else {
                    KeyState::Up
                };
                state.webview.notify_input_event(InputEvent::Keyboard(
                    KeyboardEvent::new_without_event(
                        pressed,
                        key,
                        code,
                        location,
                        state.modifiers,
                        event.repeat,
                        state.composing,
                    ),
                ));
            }
            WindowEvent::Ime(Ime::Preedit(data, _)) => {
                if !state.composing {
                    state
                        .webview
                        .notify_input_event(InputEvent::Ime(ImeEvent::Composition(
                            CompositionEvent {
                                state: CompositionState::Start,
                                data: String::new(),
                            },
                        )));
                    state.composing = true;
                }
                state
                    .webview
                    .notify_input_event(InputEvent::Ime(ImeEvent::Composition(CompositionEvent {
                        state: CompositionState::Update,
                        data,
                    })));
            }
            WindowEvent::Ime(Ime::Commit(data)) => {
                state
                    .webview
                    .notify_input_event(InputEvent::Ime(ImeEvent::Composition(CompositionEvent {
                        state: CompositionState::End,
                        data,
                    })));
                state.composing = false;
            }
            WindowEvent::Ime(Ime::Disabled) => {
                state
                    .webview
                    .notify_input_event(InputEvent::Ime(ImeEvent::Dismissed));
                state.composing = false;
            }
            _ => {}
        }
        state.servo.spin_event_loop();
    }

    fn exiting(&mut self, _: &ActiveEventLoop) {
        self.state.take();
    }
}

pub fn run(bridge: &mut Bridge, options: WindowOptions) -> Result<(), Box<dyn std::error::Error>> {
    let event_loop = EventLoop::<Wake>::with_user_event().build()?;
    let mut app = App {
        url: bridge.entry_url.clone(),
        options,
        registry: RefCell::new(Some(bridge.take_registry()?)),
        waker: Waker(event_loop.create_proxy()),
        state: None,
        error: None,
    };
    event_loop.run_app(&mut app)?;
    if let Some(error) = app.error {
        return Err(error.into());
    }
    Ok(())
}
