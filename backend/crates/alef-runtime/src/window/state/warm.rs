// SPDX-License-Identifier: MPL-2.0
// Event-loop integration follows Servo 0.6's MPL-2.0 winit example.
//! The windowed warm spare: the event loop, a hidden window and a GL context exist before the
//! application is known (`window::run_warm`); when the assignment arrives, the same event loop
//! continues as a normal application whose first window adopts the prepared one. Lives under
//! `state` to keep the `window` directory within its entry limit.
use std::error::Error;
use std::io;
use std::rc::Rc;
use std::sync::{Arc, Mutex};

use alef_core::registry::window::geometry;
use alef_core::security::window::WindowDef;
use servo::RenderingContext;
use winit::application::ApplicationHandler;
use winit::dpi::LogicalSize;
use winit::event::StartCause;
use winit::event_loop::{ActiveEventLoop, ControlFlow, EventLoop, EventLoopProxy};
use winit::window::{Window, WindowId};

use crate::ui::integration::menu::Menus;
use crate::ui::Wake;
use crate::window::{app_from, App, WarmAssignment, WarmPrepare};

/// What the assignment returns to the spare: the application, or why it refused (the client then
/// starts cold). Moves from the worker thread to the event loop: every field must be `Send`.
type Assignment = Result<WarmAssignment, Box<dyn Error + Send>>;

/// The window and GL context a warm spare prepared before the application was known.
pub(in crate::window) struct PreparedWindow {
    pub(in crate::window) window: Rc<Window>,
    pub(in crate::window) rendering: Rc<dyn RenderingContext>,
}

/// Creates the hidden window of the spare and its GL context (the interval the cold start spends
/// on the same work). The geometry of the application is applied only after the assignment.
pub(in crate::window) fn prepare_hidden(
    event_loop: &ActiveEventLoop,
    quiet: bool,
) -> io::Result<PreparedWindow> {
    let window = Rc::new(
        event_loop
            .create_window(Window::default_attributes().with_visible(false))
            .map_err(io::Error::other)?,
    );
    let rendering = super::rendering::new(event_loop, &window, quiet)?;
    rendering.make_current().map_err(|error| {
        io::Error::other(format!("Cannot activate Servo graphics context: {error:?}"))
    })?;
    crate::timing::mark("context-ready");
    Ok(PreparedWindow { window, rendering })
}

/// Whether a window takes the prepared one: only the first window of the application can; the
/// later ones are created as usual.
pub(in crate::window) fn adopts<T>(prepared: &Option<T>, first: bool) -> bool {
    first && prepared.is_some()
}

/// What the prepared window becomes once the application is known: everything `open` would have
/// put into the window attributes, as setters on the ready window.
pub(in crate::window) struct Reconfiguration {
    pub(in crate::window) size: LogicalSize<f64>,
    pub(in crate::window) resizable: bool,
    pub(in crate::window) decorations: bool,
    pub(in crate::window) min_size: Option<LogicalSize<f64>>,
    pub(in crate::window) max_size: Option<LogicalSize<f64>>,
    pub(in crate::window) maximized: bool,
}

/// The setters a prepared window receives for a definition resolved into `placement`. Pure, so the
/// defaults stay the same as at creation and stay testable without a window.
pub(in crate::window) fn reconfiguration(
    definition: &WindowDef,
    placement: &geometry::Placement,
    start_maximized: bool,
    quiet: bool,
) -> Reconfiguration {
    Reconfiguration {
        size: LogicalSize::new(placement.width, placement.height),
        resizable: definition.resizable.unwrap_or(true),
        decorations: definition.decorations.unwrap_or(true),
        min_size: placement.min_size.map(|(w, h)| LogicalSize::new(w, h)),
        max_size: placement.max_size.map(|(w, h)| LogicalSize::new(w, h)),
        // In quiet mode maximizing is only what the document is told (see `state::Pretended`).
        maximized: start_maximized && !quiet,
    }
}

/// The slot the worker thread of a spare hands the assignment to the event loop through.
#[derive(Default)]
pub(in crate::window) struct Slot(Mutex<Option<Assignment>>);

impl Slot {
    fn store(&self, assignment: Assignment) {
        *self.lock() = Some(assignment);
    }

    fn take(&self) -> Option<Assignment> {
        self.lock().take()
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Option<Assignment>> {
        self.0
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
    }
}

/// The event-loop handler of a warm spare: prepares the window, waits for the assignment without
/// spending processor time, and once it arrives builds the usual application and delegates every
/// event to it.
pub(in crate::window) struct WarmApp {
    prepare: Option<WarmPrepare>,
    slot: Arc<Slot>,
    proxy: EventLoopProxy<Wake>,
    menus: Menus,
    quiet: bool,
    prepared: Option<PreparedWindow>,
    app: Option<App>,
    pub(in crate::window) error: Option<Box<dyn Error>>,
    spawned: bool,
}

/// The handler of a spare for `run_warm`: it takes the menus before the event loop is built, so
/// the menu accelerators stay shared with the message hook of the platform.
pub(in crate::window) fn warm_app(
    prepare: WarmPrepare,
    menus: Menus,
    quiet: bool,
    event_loop: &EventLoop<Wake>,
) -> WarmApp {
    WarmApp {
        prepare: Some(prepare),
        slot: Arc::new(Slot::default()),
        proxy: event_loop.create_proxy(),
        menus,
        quiet,
        prepared: None,
        app: None,
        error: None,
        spawned: false,
    }
}

impl WarmApp {
    fn delegate_resumed(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(app) = self.app.as_mut() {
            ApplicationHandler::resumed(app, event_loop);
        }
    }
}

impl ApplicationHandler<Wake> for WarmApp {
    fn resumed(&mut self, event_loop: &ActiveEventLoop) {
        if self.spawned {
            self.delegate_resumed(event_loop);
            return;
        }
        self.spawned = true;
        if self.prepared.is_none() {
            match prepare_hidden(event_loop, self.quiet) {
                Ok(prepared) => self.prepared = Some(prepared),
                Err(error) => {
                    self.error = Some(error.into());
                    event_loop.exit();
                    return;
                }
            }
        }
        let Some(prepare) = self.prepare.take() else {
            return;
        };
        let slot = self.slot.clone();
        let proxy = self.proxy.clone();
        if let Err(error) = std::thread::Builder::new()
            .name("alef-warm-assignment".to_owned())
            .spawn(move || {
                let assignment = (prepare.assignment)();
                slot.store(assignment);
                let _ = proxy.send_event(Wake);
            })
        {
            self.error = Some(io::Error::other(error).into());
            event_loop.exit();
        }
    }

    fn user_event(&mut self, event_loop: &ActiveEventLoop, event: Wake) {
        if let Some(app) = self.app.as_mut() {
            ApplicationHandler::user_event(app, event_loop, event);
            return;
        }
        match self.slot.take() {
            None => {}
            Some(Ok(assignment)) => {
                crate::timing::mark("assigned");
                let WarmAssignment { bridge, options } = assignment;
                let prepared = self.prepared.take();
                let menus = std::mem::take(&mut self.menus);
                match app_from(bridge, options, menus, self.proxy.clone(), prepared) {
                    Ok(mut app) => {
                        // The application opens its windows now; the first one adopts the window
                        // and context prepared before the assignment.
                        ApplicationHandler::resumed(&mut app, event_loop);
                        ApplicationHandler::user_event(&mut app, event_loop, event);
                        self.app = Some(app);
                    }
                    Err(error) => {
                        self.error = Some(error);
                        event_loop.exit();
                    }
                }
            }
            Some(Err(error)) => {
                // The assignment failed or was refused: the spare served its purpose and the
                // client starts cold. The window is never shown.
                self.error = Some(error.into());
                event_loop.exit();
            }
        }
    }

    fn new_events(&mut self, event_loop: &ActiveEventLoop, cause: StartCause) {
        if let Some(app) = self.app.as_mut() {
            ApplicationHandler::new_events(app, event_loop, cause);
        }
    }

    fn about_to_wait(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(app) = self.app.as_mut() {
            ApplicationHandler::about_to_wait(app, event_loop);
            return;
        }
        // A spare without its assignment has no windows, no timers and no animation.
        event_loop.set_control_flow(ControlFlow::Wait);
    }

    fn window_event(
        &mut self,
        event_loop: &ActiveEventLoop,
        window_id: WindowId,
        event: winit::event::WindowEvent,
    ) {
        if let Some(app) = self.app.as_mut() {
            ApplicationHandler::window_event(app, event_loop, window_id, event);
        }
        // Events of the still hidden spare window (a resize of the default size, say) concern no
        // application yet.
    }

    fn exiting(&mut self, event_loop: &ActiveEventLoop) {
        if let Some(app) = self.app.as_mut() {
            ApplicationHandler::exiting(app, event_loop);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn definition() -> WindowDef {
        crate::window::WindowOptions::new("App", Vec::new())
            .windows
            .into_iter()
            .next()
            .expect("the default options have one window")
    }

    fn placement(width: f64, height: f64) -> geometry::Placement {
        geometry::Placement {
            width,
            height,
            position: None,
            min_size: None,
            max_size: None,
        }
    }

    #[test]
    fn only_the_first_window_adopts_the_prepared_one() {
        let nothing: Option<u8> = None;
        let ready: Option<u8> = Some(1);
        assert!(!adopts(&nothing, true), "nothing prepared, nothing adopted");
        assert!(!adopts(&nothing, false));
        assert!(!adopts(&ready, false), "later windows are created as usual");
        assert!(adopts(&ready, true));
    }

    #[test]
    fn the_definition_becomes_setters_with_the_same_defaults_as_creation() {
        let changes = reconfiguration(&definition(), &placement(1200.0, 800.0), false, false);
        assert_eq!(changes.size, LogicalSize::new(1200.0, 800.0));
        assert!(changes.resizable && changes.decorations);
        assert_eq!(changes.min_size, None);
        assert_eq!(changes.max_size, None);
        assert!(!changes.maximized);
    }

    #[test]
    fn quiet_never_maximizes_for_real_and_limits_become_sizes() {
        let mut bounds = placement(1200.0, 800.0);
        bounds.min_size = Some((300.0, 200.0));
        bounds.max_size = Some((1600.0, 900.0));
        let quiet = reconfiguration(&definition(), &bounds, true, true);
        assert_eq!(quiet.min_size, Some(LogicalSize::new(300.0, 200.0)));
        assert_eq!(quiet.max_size, Some(LogicalSize::new(1600.0, 900.0)));
        assert!(
            !quiet.maximized,
            "a quiet window is never maximized natively"
        );
        let visible = reconfiguration(&definition(), &bounds, true, false);
        assert!(visible.maximized);
    }

    #[test]
    fn the_definition_overrides_the_defaults() {
        let mut explicit = definition();
        explicit.resizable = Some(false);
        explicit.decorations = Some(false);
        let changes = reconfiguration(&explicit, &placement(800.0, 600.0), false, false);
        assert!(!changes.resizable && !changes.decorations);
    }
}
