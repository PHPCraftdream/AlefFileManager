// SPDX-License-Identifier: MPL-2.0
// Event-loop integration follows Servo 0.6's MPL-2.0 winit example.
use std::io;

use super::App;
use crate::ui::Wake;
use euclid::Scale;
use servo::{
    Code, CompositionEvent, CompositionState, DevicePoint, ImeEvent, InputEvent, Key, KeyState,
    KeyboardEvent, Location, Modifiers, MouseButton, MouseButtonAction, MouseButtonEvent,
    MouseLeftViewportEvent, MouseMoveEvent, NamedKey, WheelEvent,
};
use winit::application::ApplicationHandler;
use winit::event::{ElementState, Ime, StartCause, WindowEvent};
use winit::event_loop::{ActiveEventLoop, ControlFlow};
use winit::keyboard::{Key as WinitKey, KeyLocation, PhysicalKey};
use winit::window::{CursorIcon, Icon, WindowId};

impl App {
    /// One pass of work on every window: requests of the documents, Servo, the first picture,
    /// what changed, close requests that were not answered.
    fn tick(&mut self, event_loop: &ActiveEventLoop) {
        self.process_requests(event_loop);
        if let Some(state) = self.windows.first() {
            state.servo.spin_event_loop();
        }
        for state in &mut self.windows {
            // A window that is never shown gets no redraw requests from the system.
            let paint = |state: &mut super::state::State| {
                if state.quiet.is_some() && state.revealed && state.frame_ready.get() {
                    state.redraw()
                } else {
                    Ok(())
                }
            };
            if let Err(error) = state
                .try_reveal()
                .and_then(|()| paint(state))
                .and_then(|()| state.publish_snapshot())
            {
                self.error.get_or_insert(error);
                event_loop.exit();
            }
        }
        self.expire_close_requests(event_loop);
    }

    fn detach(&mut self) {
        for state in &self.windows {
            let (sessions, window) = (self.sessions.clone(), state.window_id);
            self.ids.unbind(window);
            self.runtime
                .spawn(async move { sessions.close_window(window).await });
        }
        self.handle.detach();
        self.requests.close();
        while self.requests.try_recv().is_ok() {}
        self.windows.clear();
    }
}

impl ApplicationHandler<Wake> for App {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.started {
            return;
        }
        match image::load_from_memory(&self.icon_png)
            .map_err(io::Error::other)
            .and_then(|icon| {
                let icon = icon.to_rgba8();
                let (width, height) = icon.dimensions();
                Icon::from_rgba(icon.into_raw(), width, height).map_err(io::Error::other)
            }) {
            Ok(icon) => self.icon = Some(icon),
            Err(error) => {
                self.error = Some(error);
                event_loop.exit();
                return;
            }
        }
        for definition in self.definitions.clone() {
            if let Err(error) = self.open(event_loop, &definition) {
                self.error = Some(error);
                event_loop.exit();
                return;
            }
        }
        self.process_requests(event_loop);
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, _: Wake) {
        self.tick(event_loop);
    }

    fn new_events(&mut self, _: &ActiveEventLoop, cause: StartCause) {
        if matches!(cause, StartCause::ResumeTimeReached { .. }) {
            for state in &self.windows {
                if state.animating.get() {
                    state.window.request_redraw();
                }
            }
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        self.tick(event_loop);
        match self.next_wake() {
            Some(at) => event_loop.set_control_flow(ControlFlow::WaitUntil(at)),
            None => event_loop.set_control_flow(ControlFlow::Wait),
        }
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: WindowEvent,
    ) {
        let Some(index) = self
            .windows
            .iter()
            .position(|state| state.window.id() == window_id)
        else {
            return;
        };
        if matches!(event, WindowEvent::CloseRequested) {
            self.request_close(event_loop, index);
            return;
        }
        let state = &mut self.windows[index];
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
            WindowEvent::ThemeChanged(theme) => self.handle.theme_changed(theme),
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
    }
}
