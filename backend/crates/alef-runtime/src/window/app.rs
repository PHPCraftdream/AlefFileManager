// SPDX-License-Identifier: MPL-2.0
// Event-loop integration follows Servo 0.6's MPL-2.0 winit example.
use std::cell::RefCell;
use std::collections::HashSet;
use std::io;
use std::time::Instant;

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

#[derive(Clone, Copy)]
enum FailureContext {
    Startup,
    WindowOperation(&'static str),
}

thread_local! {
    static FAILING: RefCell<HashSet<&'static str>> = RefCell::new(HashSet::new());
}

/// Says whether a failure of `operation` is to be logged: the first one is, the ones that follow
/// stay quiet until the operation succeeds again (a tick repeats a failing step every few ms).
fn first_failure(operation: &'static str, failed: bool) -> bool {
    FAILING.with(|failing| {
        let mut failing = failing.borrow_mut();
        if failed {
            failing.insert(operation)
        } else {
            failing.remove(operation);
            false
        }
    })
}

/// The line to log for a failure of `operation`, or nothing when it was reported already.
fn failure_line(operation: &'static str, error: &io::Error) -> Option<String> {
    first_failure(operation, true).then(|| format!("window {operation}: {error}"))
}

fn handle_result<T>(
    result: io::Result<T>,
    context: FailureContext,
    fatal: &mut Option<io::Error>,
) -> Option<T> {
    match result {
        Ok(value) => {
            if let FailureContext::WindowOperation(operation) = context {
                first_failure(operation, false);
            }
            Some(value)
        }
        Err(error) => {
            match context {
                FailureContext::WindowOperation(operation) => {
                    if let Some(line) = failure_line(operation, &error) {
                        eprintln!("{line}");
                    }
                }
                FailureContext::Startup => {
                    fatal.get_or_insert(error);
                }
            }
            None
        }
    }
}

impl App {
    /// One pass of work on every window: requests of the documents, Servo, the first picture,
    /// what changed, close requests that were not answered.
    fn tick(&mut self, event_loop: &ActiveEventLoop) {
        self.process_requests(event_loop);
        let windows: Vec<_> = self.windows.iter().map(|state| state.window_id).collect();
        self.shortcuts
            .tick(&self.sessions, self.handle.events(), &windows);
        self.trays
            .tick(&self.sessions, self.handle.events(), &windows);
        self.menus
            .tick(&self.sessions, self.handle.events(), &windows, &self.trays);
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
            let _ = handle_result(
                state.try_reveal(),
                FailureContext::WindowOperation("reveal"),
                &mut self.error,
            );
            let _ = handle_result(
                paint(state),
                FailureContext::WindowOperation("quiet redraw"),
                &mut self.error,
            );
            let _ = handle_result(
                state.publish_snapshot(),
                FailureContext::WindowOperation("snapshot"),
                &mut self.error,
            );
        }
        self.expire_close_requests(event_loop);
        self.deliver_drops();
        if let Some(restore) = self.restore.as_mut() {
            let now = Instant::now();
            for state in &self.windows {
                restore.observe(&state.snapshot, now);
            }
            restore.save_if_due(now);
        }
    }

    fn detach(&mut self) {
        self.trays.shutdown();
        self.menus.shutdown();
        self.shortcuts.shutdown();
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
        let icon = image::load_from_memory(&self.icon_png)
            .map_err(io::Error::other)
            .and_then(|icon| {
                let icon = icon.to_rgba8();
                let (width, height) = icon.dimensions();
                Icon::from_rgba(icon.into_raw(), width, height).map_err(io::Error::other)
            });
        let Some(icon) = handle_result(icon, FailureContext::Startup, &mut self.error) else {
            event_loop.exit();
            return;
        };
        self.icon = Some(icon);
        for definition in self.definitions.clone() {
            if handle_result(
                self.open(event_loop, &definition),
                FailureContext::Startup,
                &mut self.error,
            )
            .is_none()
            {
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
                let _ = handle_result(
                    state.redraw(),
                    FailureContext::WindowOperation("redraw"),
                    &mut self.error,
                );
            }
            WindowEvent::Resized(_) => {
                let resized = handle_result(
                    state.synchronize_viewport(),
                    FailureContext::WindowOperation("viewport resize"),
                    &mut self.error,
                )
                .unwrap_or(false);
                if resized && state.resize_pending {
                    state.wait_for_resize_frame();
                }
            }
            WindowEvent::ScaleFactorChanged { scale_factor, .. } => {
                state
                    .webview
                    .set_hidpi_scale_factor(Scale::new(scale_factor as f32));
                let _ = handle_result(
                    state.synchronize_viewport(),
                    FailureContext::WindowOperation("viewport scale"),
                    &mut self.error,
                );
            }
            WindowEvent::DroppedFile(path) => {
                if state.dropped.len() < super::host::drops::MAX_DROPPED {
                    state.dropped.push(path);
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
                            if handle_result(
                                state
                                    .window
                                    .drag_resize_window(edge)
                                    .map_err(io::Error::other),
                                FailureContext::WindowOperation("drag resize"),
                                &mut self.error,
                            )
                            .is_some()
                            {
                                state.primary_pressed = false;
                                state.snapshot_dirty = true;
                                state.native_resize_active = true;
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
                if let Some(delta) = handle_result(
                    super::input::wheel::normalize(
                        delta,
                        state.window.inner_size(),
                        state.window.scale_factor(),
                    ),
                    FailureContext::WindowOperation("scroll normalization"),
                    &mut self.error,
                ) {
                    state
                        .webview
                        .notify_input_event(InputEvent::Wheel(WheelEvent::new(
                            delta,
                            state.cursor.into(),
                        )));
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
        let _ = handle_result(
            state.publish_snapshot(),
            FailureContext::WindowOperation("snapshot"),
            &mut self.error,
        );
    }

    fn exiting(&mut self, _: &ActiveEventLoop) {
        self.remember_windows();
        #[cfg(feature = "spike-integration")]
        if let Some(error) = crate::spikes::integration::deactivate() {
            self.error.get_or_insert(error);
        }
        self.detach();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn window_failures_do_not_become_fatal() {
        for operation in [
            "drag resize",
            "scroll normalization",
            "viewport resize",
            "viewport scale",
            "redraw",
            "reveal",
            "quiet redraw",
            "snapshot",
        ] {
            for kind in [
                io::ErrorKind::Unsupported,
                io::ErrorKind::Other,
                io::ErrorKind::PermissionDenied,
            ] {
                let mut fatal = None;
                assert!(handle_result::<()>(
                    Err(io::Error::new(kind, "recoverable")),
                    FailureContext::WindowOperation(operation),
                    &mut fatal,
                )
                .is_none());
                assert!(fatal.is_none(), "{operation} must not exit the app");
            }
        }
    }

    #[test]
    fn viewport_mismatch_is_not_context_loss_and_preserves_prior_failure() {
        for operation in ["viewport resize", "redraw", "reveal", "quiet redraw"] {
            let mut fatal = None;
            assert!(handle_result::<()>(
                Err(io::Error::other(
                    "Servo surface does not match native client size",
                )),
                FailureContext::WindowOperation(operation),
                &mut fatal,
            )
            .is_none());
            assert!(fatal.is_none());
        }
        let mut fatal = Some(io::Error::other("existing startup failure"));
        assert!(handle_result::<()>(
            Err(io::Error::other(
                "Servo surface does not match native client size"
            )),
            FailureContext::WindowOperation("redraw"),
            &mut fatal,
        )
        .is_none());
        assert_eq!(fatal.unwrap().to_string(), "existing startup failure");
    }

    #[test]
    fn a_failing_operation_is_reported_once_until_it_succeeds_again() {
        let error = io::Error::other("boom");
        assert_eq!(
            failure_line("redraw", &error).as_deref(),
            Some("window redraw: boom")
        );
        assert_eq!(failure_line("redraw", &error), None);
        assert!(
            failure_line("snapshot", &error).is_some(),
            "another operation has its own turn"
        );
        let mut fatal = None;
        let ok = handle_result(Ok(1), FailureContext::WindowOperation("redraw"), &mut fatal);
        assert_eq!(ok, Some(1));
        assert!(
            failure_line("redraw", &error).is_some(),
            "a success through handle_result ends the episode"
        );
    }

    #[test]
    fn a_failure_passed_through_handle_result_is_reported_by_it() {
        let mut fatal = None;
        for _ in 0..2 {
            let result = Err::<(), _>(io::Error::other("x"));
            handle_result(
                result,
                FailureContext::WindowOperation("quiet redraw"),
                &mut fatal,
            );
        }
        assert!(
            failure_line("quiet redraw", &io::Error::other("y")).is_none(),
            "handle_result had reported it already"
        );
        assert!(fatal.is_none());
    }

    #[test]
    fn success_and_startup_failure_remain_distinct() {
        let mut fatal = None;
        assert_eq!(
            handle_result(
                Ok(7),
                FailureContext::WindowOperation("viewport resize"),
                &mut fatal,
            ),
            Some(7),
        );
        assert!(fatal.is_none());
        assert!(handle_result::<()>(
            Err(io::Error::other("context creation failed")),
            FailureContext::Startup,
            &mut fatal,
        )
        .is_none());
        assert_eq!(fatal.unwrap().to_string(), "context creation failed");
    }
}
