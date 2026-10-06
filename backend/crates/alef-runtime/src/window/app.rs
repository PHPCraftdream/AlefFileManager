// SPDX-License-Identifier: MPL-2.0
// Event-loop integration follows Servo 0.6's MPL-2.0 winit example.
use std::cell::Cell;
use std::io;
use std::rc::Rc;
use std::time::{Duration, Instant};

use super::delegate::Delegate;
use super::state::{capture_window_state, State};
use super::App;
use crate::ui::{UiRequest, Wake};
use crate::WindowAction;
use euclid::Scale;
use servo::{
    Code, CompositionEvent, CompositionState, DevicePoint, ImeEvent, InputEvent, Key, KeyState,
    KeyboardEvent, Location, Modifiers, MouseButton, MouseButtonAction, MouseButtonEvent,
    MouseLeftViewportEvent, MouseMoveEvent, NamedKey, Opts, RenderingContext, ServoBuilder,
    WebViewBuilder, WheelEvent, WindowRenderingContext,
};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, Ime, StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow};
use winit::keyboard::{Key as WinitKey, KeyLocation, PhysicalKey};
use winit::raw_window_handle::{HasDisplayHandle, HasWindowHandle};
use winit::window::{CursorIcon, Icon, Window, WindowId};

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
        let attributes = match self.options.min_size {
            Some((width, height)) => {
                attributes.with_min_inner_size(winit::dpi::LogicalSize::new(width, height))
            }
            None => attributes,
        };
        let attributes = match self.options.max_size {
            Some((width, height)) => {
                attributes.with_max_inner_size(winit::dpi::LogicalSize::new(width, height))
            }
            None => attributes,
        };
        let attributes = super::platform::apply_window_icon(attributes, icon);
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
            .preferences(crate::spikes::origin::preferences())
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
        let window_id = self.windows.allocate();
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
        self.windows.bind(webview.id(), window_id);
        webview.show();
        webview.focus();
        servo.spin_event_loop();
        #[cfg(feature = "spike-integration")]
        crate::spikes::integration::activate(&window, self.waker.0.clone());
        let snapshot = capture_window_state(&window);
        Ok(State {
            webview,
            servo,
            rendering,
            window,
            animating,
            events: self.handle.events().clone(),
            window_id,
            cursor: DevicePoint::zero(),
            modifiers: Modifiers::empty(),
            composing: false,
            next_frame: Instant::now() + Duration::from_millis(16),
            primary_pressed: false,
            snapshot,
            snapshot_dirty: true,
            published_revision: None,
            page_ready,
            resize_hover: None,
            frame_ready,
            // BEGIN M0.2 multiwindow spike
            presents: Rc::new(Cell::new(0)),
            // END M0.2 multiwindow spike
            resize_pending: false,
            native_resize_active: false,
            wake_gen: self.waker.1.clone(),
        })
    }

    fn process_requests(&mut self, event_loop: &ActiveEventLoop) {
        let Some(state) = self.state.as_mut() else {
            return;
        };
        #[cfg(feature = "spike-integration")]
        crate::spikes::integration::poll(state.window.as_ref());
        while let Ok(request) = self.requests.try_recv() {
            match request {
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
        if let Some(state) = &self.state {
            let (sessions, window) = (self.sessions.clone(), state.window_id);
            self.windows.unbind(window);
            self.runtime
                .spawn(async move { sessions.close_window(window).await });
        }
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
        // BEGIN M0.2 multiwindow spike (docs/stages/m0-spikes.md)
        self.spike.resumed(self.url.clone());
        // END M0.2 multiwindow spike
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
        // BEGIN M0.2 multiwindow spike: drive the automated multiwindow scenario
        if self.spike.enabled() {
            self.spike
                .tick(event_loop, self.state.as_ref().map(State::spike_view));
            if let Some(reason) = self.spike.exit_error() {
                self.error = Some(io::Error::other(reason));
                event_loop.exit();
            }
        }
        // END M0.2 multiwindow spike
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        // BEGIN M0.2 multiwindow spike: events of spike-owned windows never reach the single-window path
        if self.spike.window_event(window_id, &event) {
            return;
        }
        // END M0.2 multiwindow spike
        if matches!(event, WindowEvent::CloseRequested) {
            #[cfg(feature = "spike-integration")]
            if let Some(error) = crate::spikes::integration::deactivate() {
                self.error.get_or_insert(error);
            }
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
                let resized = match state.synchronize_viewport() {
                    Ok(resized) => resized,
                    Err(error) => {
                        self.error = Some(error);
                        event_loop.exit();
                        return;
                    }
                };
                if resized && state.resize_pending {
                    state.wait_for_resize_frame();
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
                match super::input::wheel::normalize(
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
        #[cfg(feature = "spike-integration")]
        if let Some(error) = crate::spikes::integration::deactivate() {
            self.error.get_or_insert(error);
        }
        self.detach();
        // BEGIN M0.2 multiwindow spike: drop spike windows while the event loop is alive
        self.spike.shutdown();
        // END M0.2 multiwindow spike
    }
}

// BEGIN M0.2 multiwindow spike (docs/stages/m0-spikes.md)
impl State {
    fn spike_view(&self) -> crate::spikes::multiwindow::PrimaryView {
        crate::spikes::multiwindow::PrimaryView {
            servo: self.servo.clone(),
            webview: self.webview.clone(),
            rendering: Rc::clone(&self.rendering),
            window: Rc::clone(&self.window),
            page_ready: Rc::clone(&self.page_ready),
            presents: Rc::clone(&self.presents),
            wake_gen: std::sync::Arc::clone(&self.wake_gen),
        }
    }
}
// END M0.2 multiwindow spike
