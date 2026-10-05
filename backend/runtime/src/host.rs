// SPDX-License-Identifier: MPL-2.0
// Event-loop integration follows Servo 0.6's MPL-2.0 winit example.
use std::cell::{Cell, RefCell};
use std::io;
use std::rc::{Rc, Weak};
use std::time::{Duration, Instant};

use crate::ui::{event_json, event_script, UiRequest, Wake, WINDOW_STATE_EVENT};
use crate::{Bridge, RuntimeHandle, WindowAction, WindowState};
use euclid::Scale;
use serde_json::Value;
use servo::protocol_handler::ProtocolRegistry;
use servo::{
    Code, CompositionEvent, CompositionState, ConsoleLogLevel, DevicePoint, EventLoopWaker,
    ImeEvent, InputEvent, Key, KeyState, KeyboardEvent, LoadStatus, Location, Modifiers,
    MouseButton, MouseButtonAction, MouseButtonEvent, MouseLeftViewportEvent, MouseMoveEvent,
    NamedKey, NavigationRequest, Opts, RenderingContext, Servo, ServoBuilder, WebView,
    WebViewBuilder, WebViewDelegate, WheelEvent, WindowRenderingContext,
};
use tokio::sync::mpsc;
use url::Url;
use winit::application::ApplicationHandler;
use winit::event::{ElementState, Ime, StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::keyboard::{Key as WinitKey, KeyLocation, PhysicalKey};
use winit::raw_window_handle::{HasDisplayHandle, HasWindowHandle};
use winit::window::{CursorIcon, Icon, ResizeDirection, Window, WindowId};
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
    page_ready: Rc<Cell<bool>>,
    frame_ready: Rc<Cell<bool>>,
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
        self.page_ready.set(status == LoadStatus::Complete);
        if status == LoadStatus::Complete {
            if let Some(window) = self.window.upgrade() {
                window.request_redraw();
            }
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
    primary_pressed: bool,
    snapshot: WindowState,
    snapshot_dirty: bool,
    published_revision: Option<u32>,
    state_event_in_flight: Rc<Cell<bool>>,
    page_ready: Rc<Cell<bool>>,
    resize_hover: Option<ResizeDirection>,
    frame_ready: Rc<Cell<bool>>,
    resize_pending: bool,
    native_resize_active: bool,
}

impl State {
    fn synchronize_viewport(&mut self) -> io::Result<bool> {
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
        self.webview.paint();
        self.rendering.present();
        self.next_frame = Instant::now() + Duration::from_millis(16);
        Ok(())
    }

    fn resize_edge(&self) -> Option<ResizeDirection> {
        if !self.window.is_resizable()
            || self.window.is_decorated()
            || self.window.is_maximized()
            || !self.snapshot.supports_drag_resize
        {
            return None;
        }
        crate::window_frame::resize_hit(
            self.cursor,
            self.window.inner_size(),
            self.window.scale_factor(),
        )
    }

    fn update_resize_cursor(&mut self) {
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

    fn refresh_snapshot(&mut self) -> io::Result<()> {
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

    fn publish_snapshot(&mut self) -> io::Result<()> {
        self.refresh_snapshot()?;
        if !self.page_ready.get()
            || self.state_event_in_flight.get()
            || self.published_revision == Some(self.snapshot.revision)
        {
            return Ok(());
        }
        let json = event_json(WINDOW_STATE_EVENT, &self.snapshot)?;
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

    fn window_action(&mut self, action: WindowAction) -> io::Result<Value> {
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

fn capture_window_state(window: &Window) -> WindowState {
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
        supports_drag_resize: crate::platform::SUPPORTS_NATIVE_RESIZE,
    }
}

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
}

impl App {
    fn initialize(
        &self,
        event_loop: &ActiveEventLoop,
    ) -> Result<State, Box<dyn std::error::Error>> {
        let icon = image::load_from_memory(&self.options.icon_png)?.to_rgba8();
        let (width, height) = icon.dimensions();
        let icon = Icon::from_rgba(icon.into_raw(), width, height)?;
        let attributes = Window::default_attributes()
            .with_title(&self.options.title)
            .with_inner_size(winit::dpi::LogicalSize::new(
                self.options.width,
                self.options.height,
            ))
            .with_decorations(self.options.decorations)
            .with_resizable(self.options.resizable);
        let attributes = crate::platform::apply_window_icon(attributes, icon);
        let window = Rc::new(event_loop.create_window(attributes)?);
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
        let page_ready = Rc::new(Cell::new(false));
        let frame_ready = Rc::new(Cell::new(false));
        let delegate = Rc::new(Delegate {
            window: Rc::downgrade(&window),
            entry_url: self.url.clone(),
            title: self.options.title.clone(),
            animating: animating.clone(),
            page_ready: page_ready.clone(),
            frame_ready: frame_ready.clone(),
        });
        let webview = WebViewBuilder::new(&servo, rendering.clone())
            .url(self.url.clone())
            .hidpi_scale_factor(Scale::new(window.scale_factor() as f32))
            .delegate(delegate)
            .build();
        webview.show();
        webview.focus();
        servo.spin_event_loop();
        let snapshot = capture_window_state(&window);
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
            primary_pressed: false,
            snapshot,
            snapshot_dirty: true,
            published_revision: None,
            state_event_in_flight: Rc::new(Cell::new(false)),
            page_ready,
            resize_hover: None,
            frame_ready,
            resize_pending: false,
            native_resize_active: false,
        })
    }

    fn process_requests(&mut self, event_loop: &ActiveEventLoop) {
        let Some(state) = self.state.as_mut() else {
            return;
        };
        while let Ok(request) = self.requests.try_recv() {
            match request {
                UiRequest::Emit { json, reply } => {
                    if reply.canceled() {
                        continue;
                    }
                    state
                        .webview
                        .evaluate_javascript(event_script(&json), move |result| {
                            reply.finish(result.map(|_| Value::Null).map_err(|error| {
                                io::Error::other(format!(
                                    "Browser event dispatch failed: {error:?}"
                                ))
                            }));
                        });
                }
                UiRequest::Window { action, reply } => {
                    if reply.canceled() {
                        continue;
                    }
                    let closing = matches!(&action, WindowAction::Close);
                    reply.finish(state.window_action(action));
                    if closing {
                        event_loop.exit();
                        break;
                    }
                }
            }
        }
    }

    fn detach(&mut self) {
        self.handle.detach();
        self.requests.close();
        while self.requests.try_recv().is_ok() {}
        self.state.take();
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
        self.process_requests(event_loop);
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, _: Wake) {
        self.process_requests(event_loop);
        if let Some(state) = self.state.as_mut() {
            state.servo.spin_event_loop();
            if let Err(error) = state.publish_snapshot() {
                self.error = Some(error);
                event_loop.exit();
            }
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
        self.process_requests(event_loop);
        if let Some(state) = self.state.as_mut() {
            state.servo.spin_event_loop();
            if let Err(error) = state.publish_snapshot() {
                eprintln!("Window state capture failed: {error}");
                event_loop.exit();
            }
            if state.animating.get() {
                event_loop.set_control_flow(ControlFlow::WaitUntil(state.next_frame));
            } else {
                event_loop.set_control_flow(ControlFlow::Wait);
            }
        }
    }

    fn window_event(&mut self, event_loop: &ActiveEventLoop, _: WindowId, event: WindowEvent) {
        if matches!(event, WindowEvent::CloseRequested) {
            self.detach();
            event_loop.exit();
            return;
        }
        let Some(state) = self.state.as_mut() else {
            return;
        };
        if matches!(
            &event,
            WindowEvent::Moved(_)
                | WindowEvent::Resized(_)
                | WindowEvent::ScaleFactorChanged { .. }
                | WindowEvent::Focused(_)
                | WindowEvent::Occluded(_)
        ) {
            state.snapshot_dirty = true;
        }
        match event {
            WindowEvent::RedrawRequested => {
                if let Err(error) = state.redraw() {
                    self.error = Some(error);
                    event_loop.exit();
                }
            }
            WindowEvent::Resized(_) => {
                if let Err(error) = state.synchronize_viewport() {
                    self.error = Some(error);
                    event_loop.exit();
                }
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                state
                    .webview
                    .set_hidpi_scale_factor(Scale::new(scale_factor as f32));
                if let Err(error) = state.synchronize_viewport() {
                    self.error = Some(error);
                    event_loop.exit();
                }
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
                state.update_resize_cursor();
                state
                    .webview
                    .notify_input_event(InputEvent::MouseMove(MouseMoveEvent::new(
                        state.cursor.into(),
                    )));
            }
            WindowEvent::CursorLeft { .. } => {
                if !state.native_resize_active {
                    state.resize_hover = None;
                    state.window.set_cursor(CursorIcon::Default);
                }
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
                if button == winit::event::MouseButton::Left {
                    state.primary_pressed = pressed == ElementState::Pressed;
                    if pressed == ElementState::Released {
                        state.native_resize_active = false;
                        state.update_resize_cursor();
                    }
                    if pressed == ElementState::Pressed {
                        if let Some(edge) = state.resize_edge() {
                            match state.window.drag_resize_window(edge) {
                                Ok(()) => {
                                    state.primary_pressed = false;
                                    state.snapshot_dirty = true;
                                    state.native_resize_active = true;
                                }
                                Err(error) => {
                                    self.error = Some(io::Error::other(error));
                                    event_loop.exit();
                                }
                            }
                            return;
                        }
                    }
                }
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
                match crate::wheel::normalize(
                    delta,
                    state.window.inner_size(),
                    state.window.scale_factor(),
                ) {
                    Ok(delta) => {
                        state
                            .webview
                            .notify_input_event(InputEvent::Wheel(WheelEvent::new(
                                delta,
                                state.cursor.into(),
                            )));
                    }
                    Err(error) => {
                        self.error = Some(error);
                        event_loop.exit();
                    }
                }
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
        if let Err(error) = state.publish_snapshot() {
            self.error = Some(error);
            event_loop.exit();
        }
    }

    fn exiting(&mut self, _: &ActiveEventLoop) {
        self.detach();
    }
}

pub fn run(bridge: &mut Bridge, options: WindowOptions) -> Result<(), Box<dyn std::error::Error>> {
    let event_loop = EventLoop::<Wake>::with_user_event().build()?;
    let handle = bridge.handle();
    handle.attach(event_loop.create_proxy());
    let mut app = App {
        url: bridge.entry_url.clone(),
        options,
        registry: RefCell::new(Some(bridge.take_registry()?)),
        waker: Waker(event_loop.create_proxy()),
        state: None,
        error: None,
        handle,
        requests: bridge.take_requests()?,
    };
    event_loop.run_app(&mut app)?;
    if let Some(error) = app.error {
        return Err(error.into());
    }
    Ok(())
}
