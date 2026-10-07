// SPDX-License-Identifier: MPL-2.0
// Window creation follows Servo 0.6's MPL-2.0 winit example.
//! All windows of the application: opening and closing them, and what a document may ask of them.
use std::cell::Cell;
use std::io;
use std::rc::Rc;
use std::time::{Duration, Instant};

use alef_core::{
    registry::window::{geometry, UiCall, WindowCall, WindowOp},
    security::window::{WindowDef, WindowPosition},
};
use euclid::Scale;
use serde_json::Value;
use servo::{
    DevicePoint, Modifiers, Opts, RenderingContext, ServoBuilder, WebViewBuilder,
    WindowRenderingContext,
};
use url::Url;
use winit::dpi::LogicalSize;
use winit::event_loop::ActiveEventLoop;
use winit::raw_window_handle::{HasDisplayHandle, HasWindowHandle};
use winit::window::Window;

use super::displays::{physical_size, Displays};
use super::events;
use super::ops::io_error;
use crate::ui::{event_json, UiRequest};
use crate::window::delegate::Delegate;
use crate::window::platform;
use crate::window::state::{capture_info, PendingClose, Pretended, State};
use crate::window::App;

/// How long a document may take to answer `window.close-requested` before the window closes anyway.
const CLOSE_ANSWER_LIMIT: Duration = Duration::from_secs(3);

fn not_found(message: String) -> io::Error {
    io::Error::new(io::ErrorKind::NotFound, message)
}

fn to_json<T: serde::Serialize>(value: &T) -> io::Result<Value> {
    serde_json::to_value(value).map_err(io::Error::other)
}

impl App {
    /// The document a window opens: the entry document for the first window, otherwise a path of
    /// the same application (same scheme, host and port), carrying the same capability.
    fn url_for(&self, definition: &WindowDef, first: bool) -> io::Result<Url> {
        if first {
            return Ok(self.url.clone());
        }
        let mut url = self
            .url
            .join(&definition.url)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        let same = (url.scheme(), url.host_str(), url.port())
            == (self.url.scheme(), self.url.host_str(), self.url.port());
        if !same {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "A window opens a path of the application, not another host",
            ));
        }
        url.set_fragment(self.url.fragment());
        Ok(url)
    }

    /// Opens a window, hidden until its page has something to show. The first one also starts
    /// Servo, which the others share.
    pub(in crate::window) fn open(
        &mut self,
        event_loop: &ActiveEventLoop,
        definition: &WindowDef,
    ) -> io::Result<u64> {
        let first = !self.started;
        if !first && self.windows.is_empty() {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "The application is closing",
            ));
        }
        if self
            .windows
            .iter()
            .any(|state| state.label == definition.label)
        {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                format!("A window labelled {} exists", definition.label),
            ));
        }
        let url = self.url_for(definition, first)?;
        let displays = Displays::collect(
            event_loop.available_monitors(),
            event_loop.primary_monitor(),
        );
        let infos = displays.infos();
        // A window that asks for it opens where it was the last time.
        let (restored, start_maximized) = match self
            .restore
            .as_ref()
            .filter(|_| definition.restore)
            .and_then(|restore| restore.apply(definition, &infos))
        {
            Some((restored, maximized)) => (restored, maximized),
            None => (definition.clone(), false),
        };
        if definition.restore {
            if let Some(restore) = self.restore.as_mut() {
                restore.track(&definition.label);
            }
        }
        let definition = &restored;
        let cursor = displays.cursor();
        let placement = geometry::place(definition, &infos, cursor).map_err(io_error)?;
        let target = geometry::pick(&infos, definition.monitor, cursor)
            .and_then(|picked| infos.iter().position(|info| std::ptr::eq(info, picked)));
        let title = definition
            .title
            .clone()
            .unwrap_or_else(|| self.title.clone());
        let mut attributes = Window::default_attributes()
            .with_title(&title)
            .with_inner_size(physical_size(
                (placement.width, placement.height),
                displays.scale(target),
            ))
            .with_decorations(definition.decorations.unwrap_or(true))
            .with_resizable(definition.resizable.unwrap_or(true))
            // Shown by `State::try_reveal` once there is a first picture; until then the window
            // would be an unpainted rectangle.
            .with_visible(false)
            // In quiet mode maximizing is only what the document is told (see `state::Pretended`).
            .with_maximized(start_maximized && !self.quiet);
        if let Some(position) = placement
            .position
            .and_then(|point| displays.physical_position(point, target))
        {
            attributes = attributes.with_position(position);
        }
        if let Some((width, height)) = placement.min_size {
            attributes = attributes.with_min_inner_size(LogicalSize::new(width, height));
        }
        if let Some((width, height)) = placement.max_size {
            attributes = attributes.with_max_inner_size(LogicalSize::new(width, height));
        }
        let icon = self
            .icon
            .clone()
            .ok_or_else(|| io::Error::other("The window icon is not loaded"))?;
        let attributes = platform::apply_window_icon(attributes, icon);
        let window = Rc::new(
            event_loop
                .create_window(attributes)
                .map_err(io::Error::other)?,
        );
        // The outer frame (title bar, borders) is known only now: centre by it.
        if matches!(definition.position, WindowPosition::Center) {
            let area = target
                .and_then(|index| infos.get(index))
                .map(|info| info.work_area);
            if let Some(area) = area {
                let (scale, outer) = (window.scale_factor(), window.outer_size());
                let at = geometry::center(
                    &area,
                    f64::from(outer.width) / scale,
                    f64::from(outer.height) / scale,
                );
                if let Some(position) = displays.physical_position(at, target) {
                    window.set_outer_position(position);
                }
            }
        }
        window.set_ime_allowed(true);
        if first {
            if let Some(theme) = window.theme() {
                self.handle.set_theme(theme);
            }
        }
        let rendering = Rc::new(
            WindowRenderingContext::new(
                event_loop.display_handle().map_err(io::Error::other)?,
                window.window_handle().map_err(io::Error::other)?,
                window.inner_size(),
            )
            .map_err(|error| {
                io::Error::other(format!("Cannot create Servo graphics context: {error:?}"))
            })?,
        );
        rendering.make_current().map_err(|error| {
            io::Error::other(format!("Cannot activate Servo graphics context: {error:?}"))
        })?;
        let servo = match self.windows.first() {
            Some(state) => state.servo.clone(),
            None => {
                let servo =
                    ServoBuilder::default()
                        .opts(Opts {
                            multiprocess: false,
                            temporary_storage: true,
                            ..Opts::default()
                        })
                        .preferences(crate::spikes::origin::preferences())
                        .protocol_registry(self.registry.borrow_mut().take().ok_or_else(|| {
                            io::Error::other("Protocol registry already consumed")
                        })?)
                        .event_loop_waker(Box::new(self.waker.clone()))
                        .build();
                servo.setup_logging();
                servo
            }
        };
        let animating = Rc::new(Cell::new(false));
        let page_ready = Rc::new(Cell::new(false));
        let frame_ready = Rc::new(Cell::new(false));
        let content_frame = Rc::new(Cell::new(false));
        let window_id = self.ids.allocate();
        let delegate = Rc::new(Delegate {
            window: Rc::downgrade(&window),
            entry_url: self.url.clone(),
            title: title.clone(),
            animating: animating.clone(),
            page_ready: page_ready.clone(),
            frame_ready: frame_ready.clone(),
            content_frame: content_frame.clone(),
        });
        let webview = WebViewBuilder::new(&servo, rendering.clone())
            .url(url)
            .hidpi_scale_factor(Scale::new(window.scale_factor() as f32))
            .delegate(delegate)
            .build();
        self.ids.bind(webview.id(), window_id);
        webview.show();
        webview.focus();
        servo.spin_event_loop();
        #[cfg(feature = "spike-integration")]
        if first {
            crate::spikes::integration::activate(&window, self.waker.0.clone());
        }
        let quiet = self.quiet.then(|| Pretended {
            maximized: start_maximized,
            ..Pretended::default()
        });
        let snapshot = capture_info(
            &definition.label,
            &title,
            &window,
            &webview,
            false,
            quiet.as_ref(),
        );
        self.windows.push(State {
            webview,
            servo,
            rendering,
            window,
            animating,
            events: self.handle.events().clone(),
            window_id,
            label: definition.label.clone(),
            title,
            cursor: DevicePoint::zero(),
            modifiers: Modifiers::empty(),
            composing: false,
            next_frame: Instant::now() + Duration::from_millis(16),
            primary_pressed: false,
            snapshot,
            snapshot_dirty: true,
            published: None,
            page_ready,
            resize_hover: None,
            frame_ready,
            content_frame,
            revealed: false,
            // Asked at creation, but a window manager of X11 hears of it only when the window is mapped.
            maximize_when_shown: (start_maximized && !self.quiet).then_some(true),
            created: Instant::now(),
            ready_since: None,
            resize_pending: false,
            native_resize_active: false,
            wake_gen: self.waker.1.clone(),
            always_on_top: false,
            quiet,
            min_size: placement.min_size,
            max_size: placement.max_size,
            intercept: None,
            pending_close: None,
            dropped: Vec::new(),
        });
        self.started = true;
        Ok(window_id)
    }

    /// Index of the window `label` names, or of the window of the calling document.
    fn find(&self, caller: u64, label: Option<&str>) -> io::Result<usize> {
        match label {
            Some(label) => self
                .windows
                .iter()
                .position(|state| state.label == label)
                .ok_or_else(|| not_found(format!("No window is labelled {label}"))),
            None => self
                .windows
                .iter()
                .position(|state| state.window_id == caller)
                .ok_or_else(|| not_found("The calling window is gone".to_owned())),
        }
    }

    /// Closes a window; the application ends with its last window.
    pub(in crate::window) fn close(&mut self, event_loop: &ActiveEventLoop, index: usize) {
        if let (Some(restore), Ok(info)) =
            (self.restore.as_mut(), self.windows[index].fresh_snapshot())
        {
            restore.observe(&info, Instant::now());
            restore.flush();
        }
        let state = self.windows.remove(index);
        self.ids.unbind(state.window_id);
        let (sessions, window) = (self.sessions.clone(), state.window_id);
        self.runtime
            .spawn(async move { sessions.close_window(window).await });
        drop(state);
        if self.windows.is_empty() {
            event_loop.exit();
        }
    }

    /// The user (or `window.close()`) asks to close a window: a document that has taken over close
    /// requests is asked first, any other window closes at once.
    pub(in crate::window) fn request_close(&mut self, event_loop: &ActiveEventLoop, index: usize) {
        let window = self.windows[index].window_id;
        let interceptor = self.windows[index].intercept.is_some_and(|session| {
            self.sessions
                .current(window)
                .is_some_and(|current| current.id() == session)
        });
        if !interceptor {
            self.close(event_loop, index);
            return;
        }
        if self.windows[index].pending_close.is_some() {
            return;
        }
        self.next_close += 1;
        let id = self.next_close;
        let state = &mut self.windows[index];
        state.pending_close = Some(PendingClose {
            id,
            deadline: Instant::now() + CLOSE_ANSWER_LIMIT,
        });
        let payload = serde_json::json!({"label": state.label, "id": id});
        match event_json(events::CLOSE_REQUESTED, &payload) {
            Ok(json) => state.events.publish(Some(window), &json),
            Err(_) => self.close(event_loop, index),
        }
    }

    /// Closes the windows whose document did not answer in time.
    pub(in crate::window) fn expire_close_requests(&mut self, event_loop: &ActiveEventLoop) {
        let now = Instant::now();
        let overdue: Vec<u64> = self
            .windows
            .iter()
            .filter(|state| {
                state
                    .pending_close
                    .as_ref()
                    .is_some_and(|pending| pending.deadline <= now)
            })
            .map(|state| state.window_id)
            .collect();
        for window in overdue {
            if let Some(index) = self.windows.iter().position(|s| s.window_id == window) {
                self.close(event_loop, index);
            }
        }
    }

    /// When the loop has to wake up on its own: animation frames, hidden windows waiting for their
    /// first picture, close requests waiting for an answer.
    pub(in crate::window) fn next_wake(&self) -> Option<Instant> {
        let now = Instant::now();
        self.windows
            .iter()
            .filter_map(|state| {
                let animation = state.animating.get().then_some(state.next_frame);
                let reveal = (!state.revealed).then(|| now + crate::window::state::REVEAL_POLL);
                let close = state.pending_close.as_ref().map(|pending| pending.deadline);
                [animation, reveal, close].into_iter().flatten().min()
            })
            .chain(self.restore.as_ref().and_then(|restore| restore.due_at()))
            .min()
    }

    /// Writes down where the windows are, at the end of the application.
    pub(in crate::window) fn remember_windows(&mut self) {
        let Some(restore) = self.restore.as_mut() else {
            return;
        };
        let now = Instant::now();
        for state in &mut self.windows {
            if let Ok(info) = state.fresh_snapshot() {
                restore.observe(&info, now);
            }
        }
        restore.flush();
    }

    fn call(
        &mut self,
        event_loop: &ActiveEventLoop,
        caller: u64,
        call: WindowCall,
    ) -> io::Result<Value> {
        let own = || {
            io::Error::new(
                io::ErrorKind::InvalidInput,
                "Close interception concerns the window of the calling document only",
            )
        };
        let index = self.find(caller, call.label.as_deref())?;
        match call.op {
            WindowOp::Close => self.request_close(event_loop, index),
            WindowOp::Destroy => self.close(event_loop, index),
            WindowOp::CloseIntercept { enabled } => {
                if call.label.is_some() {
                    return Err(own());
                }
                let session = if enabled {
                    let session = self.sessions.current(caller).ok_or_else(|| {
                        io::Error::new(io::ErrorKind::BrokenPipe, "The document has no session")
                    })?;
                    Some(session.id())
                } else {
                    None
                };
                let state = &mut self.windows[index];
                state.intercept = session;
                if session.is_none() {
                    state.pending_close = None;
                }
            }
            WindowOp::CloseAnswer { id, prevent } => {
                if call.label.is_some() {
                    return Err(own());
                }
                let pending = self.windows[index]
                    .pending_close
                    .as_ref()
                    .is_some_and(|pending| pending.id == id);
                if pending {
                    self.windows[index].pending_close = None;
                    if !prevent {
                        self.close(event_loop, index);
                    }
                }
            }
            op => return self.windows[index].apply(op),
        }
        Ok(Value::Null)
    }

    fn create(
        &mut self,
        event_loop: &ActiveEventLoop,
        definition: &WindowDef,
    ) -> io::Result<Value> {
        let window = self.open(event_loop, definition)?;
        let index = self
            .windows
            .iter()
            .position(|state| state.window_id == window)
            .ok_or_else(|| io::Error::other("The new window vanished"))?;
        to_json(&self.windows[index].fresh_snapshot()?)
    }

    fn handle_request(
        &mut self,
        event_loop: &ActiveEventLoop,
        caller: u64,
        call: UiCall,
    ) -> io::Result<Value> {
        match call {
            UiCall::Window(call) => self.call(event_loop, caller, call),
            UiCall::Create(definition) => self.create(event_loop, &definition),
            UiCall::Windows => {
                let mut all = Vec::with_capacity(self.windows.len());
                for state in &mut self.windows {
                    all.push(state.fresh_snapshot()?);
                }
                to_json(&all)
            }
            UiCall::Monitors => {
                let displays = Displays::collect(
                    event_loop.available_monitors(),
                    event_loop.primary_monitor(),
                );
                to_json(&displays.infos())
            }
            UiCall::CursorPosition => {
                let displays = Displays::collect(
                    event_loop.available_monitors(),
                    event_loop.primary_monitor(),
                );
                let point = displays.cursor().ok_or_else(|| {
                    io::Error::new(
                        io::ErrorKind::Unsupported,
                        "The cursor position is not available on this platform or display",
                    )
                })?;
                to_json(&point)
            }
            // The answer comes when the user is done: `process_requests` hands it to `dialogs`.
            UiCall::Dialog(_) => Err(io::Error::other("A dialog is answered later")),
        }
    }

    /// Answers the requests the documents have queued for the UI thread.
    pub(in crate::window) fn process_requests(&mut self, event_loop: &ActiveEventLoop) {
        if self.handle.quit_requested() {
            event_loop.exit();
            return;
        }
        #[cfg(feature = "spike-integration")]
        if let Some(state) = self.windows.first() {
            crate::spikes::integration::poll(state.window.as_ref());
        }
        while let Ok(request) = self.requests.try_recv() {
            let (caller, call, reply) = match request {
                UiRequest::Ui {
                    caller,
                    call,
                    reply,
                } => (caller, call, reply),
                UiRequest::Drop {
                    window,
                    paths,
                    reply,
                } => {
                    self.file_drop(window, paths);
                    reply.finish(Ok(Value::Null));
                    continue;
                }
            };
            if reply.canceled() {
                continue;
            }
            if let UiCall::Dialog(dialog) = call {
                let parent = self
                    .find(caller, None)
                    .ok()
                    .map(|index| self.windows[index].window.as_ref());
                self.dialogs
                    .show(&self.runtime, &self.title, parent, dialog, reply);
                continue;
            }
            let result = self.handle_request(event_loop, caller, call);
            reply.finish(result);
        }
    }
}
