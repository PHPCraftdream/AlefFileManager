// SPDX-License-Identifier: MPL-2.0
// Event-loop integration follows Servo 0.6's MPL-2.0 winit example.
mod app;
mod delegate;
mod host;
mod input;
mod platform;
mod state;

use std::cell::RefCell;
use std::io;
use std::path::PathBuf;
use std::sync::Arc;

use crate::bridge::WindowRegistry;
use crate::ui::{UiRequest, Wake};
use crate::{Bridge, RuntimeHandle};
use alef_core::{
    security::window::{Length, Monitor, WindowDef, WindowPosition},
    session::session::SessionManager,
};
use delegate::Waker;
use servo::protocol_handler::ProtocolRegistry;
pub(crate) use state::timing;
use state::State;
use tokio::sync::mpsc;
use url::Url;
use winit::event_loop::EventLoop;
use winit::window::Icon;

/// What the application opens at start.
pub struct WindowOptions {
    /// Title of a window that has none of its own (the application name).
    pub title: String,
    pub icon_png: Vec<u8>,
    /// The windows opened at start. The first is the main one and opens the entry document of the
    /// bridge, whatever its `url` says. Never empty.
    pub windows: Vec<WindowDef>,
    /// The file that remembers where the windows with `restore: true` were; without it they are
    /// not remembered.
    pub state_file: Option<PathBuf>,
    /// Servo profile root (`Opts.config_dir`): web storage and the disk cache live under it.
    /// `None` keeps the Servo default.
    pub profile_dir: Option<PathBuf>,
    /// Optional loopback DevTools listener port.
    pub devtools_port: Option<u16>,
    /// The package identity of the application, named in the DevTools consent dialog.
    pub app_id: String,
}

impl WindowOptions {
    /// One window, 1200x800, centred, with the native frame.
    pub fn new(title: impl Into<String>, icon_png: Vec<u8>) -> Self {
        Self {
            title: title.into(),
            icon_png,
            state_file: None,
            profile_dir: None,
            devtools_port: None,
            app_id: String::new(),
            windows: vec![WindowDef {
                label: "main".to_owned(),
                url: "/".to_owned(),
                width: Length::Px(1200.0),
                height: Length::Px(800.0),
                min_width: None,
                min_height: None,
                max_width: None,
                max_height: None,
                monitor: Monitor::default(),
                position: WindowPosition::default(),
                restore: false,
                title: None,
                decorations: None,
                resizable: None,
            }],
        }
    }

    /// Remembers the windows that ask for it (`restore: true`) in `path`.
    pub fn remembering_windows_in(mut self, path: impl Into<PathBuf>) -> Self {
        self.state_file = Some(path.into());
        self
    }

    /// Whether the main window has the native frame.
    pub fn decorations(mut self, enabled: bool) -> Self {
        if let Some(main) = self.windows.first_mut() {
            main.decorations = Some(enabled);
        }
        self
    }
}

struct App {
    url: Url,
    title: String,
    icon_png: Vec<u8>,
    /// Package identity, carried to the DevTools consent dialog.
    app_id: String,
    icon: Option<Icon>,
    definitions: Vec<WindowDef>,
    registry: RefCell<Option<ProtocolRegistry>>,
    waker: Waker,
    /// Every open window, in the order they were opened.
    windows: Vec<State>,
    /// The first window has been opened (and Servo started).
    started: bool,
    /// End-to-end runs of `ALEF_E2E_QUIET=1`: windows are never shown (see `state::Pretended`).
    quiet: bool,
    /// Last id given to a close request.
    next_close: u64,
    error: Option<io::Error>,
    handle: RuntimeHandle,
    requests: mpsc::Receiver<UiRequest>,
    sessions: Arc<SessionManager>,
    ids: WindowRegistry,
    runtime: tokio::runtime::Handle,
    dialogs: host::dialogs::Dialogs,
    shortcuts: crate::ui::integration::shortcut::Shortcuts,
    trays: crate::ui::integration::tray::Trays,
    menus: crate::ui::integration::menu::Menus,
    restore: Option<state::restore::Restore>,
    /// A window and GL context the warm spare made before the application was known; the first
    /// window adopts them (see `state::warm`).
    prepared: Option<state::warm::PreparedWindow>,
    devtools_port: Option<u16>,
    bridge_devtools: Option<std::rc::Rc<crate::ui::devtools::DevtoolsDelegate>>,
    servo_started: bool,
    /// Per-application Servo profile root (`Opts.config_dir`).
    profile_dir: Option<PathBuf>,
    /// Every document gets the WebCrypto stand-in (the right was not granted).
    webcrypto_substitute: bool,
}

#[cfg(feature = "e2e")]
fn quiet_enabled() -> bool {
    quiet_from(&|name| std::env::var(name).ok())
}

#[cfg(feature = "e2e")]
fn quiet_from(read: &dyn Fn(&str) -> Option<String>) -> bool {
    let flag = |name| read(name).is_some_and(|value| value == "1");
    flag("ALEF_E2E") && flag("ALEF_E2E_QUIET") && !flag("ALEF_E2E_VISIBLE")
}

#[cfg(feature = "e2e")]
fn software_from(read: &dyn Fn(&str) -> Option<String>, windows: bool) -> bool {
    quiet_from(read) && (windows || read("ALEF_E2E_SOFTWARE").is_some_and(|value| value == "1"))
}

#[cfg(all(test, feature = "e2e"))]
#[test]
fn quiet_windows_do_not_depend_on_software_rendering() {
    for software in [None, Some("1"), Some("0"), Some("true")] {
        let read = |name: &str| {
            match name {
                "ALEF_E2E" | "ALEF_E2E_QUIET" => Some("1"),
                "ALEF_E2E_SOFTWARE" => software,
                "ALEF_E2E_VISIBLE" => None,
                _ => panic!("unexpected flag: {name}"),
            }
            .map(str::to_owned)
        };
        assert!(quiet_from(&read));
    }
}

#[cfg(all(test, feature = "e2e"))]
#[test]
fn quiet_windows_require_exact_flags_and_visible_windows_override_them() {
    let check = |e2e: Option<&str>, quiet: Option<&str>, visible: Option<&str>, expected| {
        let read = |name: &str| {
            match name {
                "ALEF_E2E" => e2e,
                "ALEF_E2E_QUIET" => quiet,
                "ALEF_E2E_VISIBLE" => visible,
                _ => panic!("quiet mode must not read {name}"),
            }
            .map(str::to_owned)
        };
        assert_eq!(
            quiet_from(&read),
            expected,
            "e2e={e2e:?} quiet={quiet:?} visible={visible:?}"
        );
    };
    check(None, Some("1"), None, false);
    check(Some("1"), None, None, false);
    check(Some("1"), Some("1"), None, true);
    check(Some("1"), Some("1"), Some("1"), false);
    for wrong in ["", "0", "true", "yes", "2", " 1", "1 "] {
        check(Some(wrong), Some("1"), None, false);
        check(Some("1"), Some(wrong), None, false);
        check(Some("1"), Some("1"), Some(wrong), true);
    }
}

#[cfg(all(test, feature = "e2e"))]
#[test]
fn software_rendering_requires_quiet_windows_and_windows_or_an_exact_software_flag() {
    let cases = [
        (None, None, None, None, false, false, false),
        (None, None, None, None, true, false, false),
        (None, Some("1"), Some("1"), None, true, false, false),
        (Some("1"), None, Some("1"), None, true, false, false),
        (Some("1"), Some("1"), None, None, true, true, true),
        (Some("1"), Some("1"), None, None, false, true, false),
        (Some("1"), Some("1"), Some("1"), None, false, true, true),
        (Some("1"), Some("1"), Some("1"), None, true, true, true),
        (Some("1"), Some("1"), None, Some("1"), true, false, false),
        (
            Some("1"),
            Some("1"),
            Some("1"),
            Some("1"),
            true,
            false,
            false,
        ),
        (
            Some("1"),
            Some("1"),
            Some("1"),
            Some("1"),
            false,
            false,
            false,
        ),
    ];
    let check = |e2e: Option<&str>,
                 quiet: Option<&str>,
                 software: Option<&str>,
                 visible: Option<&str>,
                 windows,
                 expected_quiet,
                 expected_software| {
        let read = |name: &str| {
            match name {
                "ALEF_E2E" => e2e,
                "ALEF_E2E_QUIET" => quiet,
                "ALEF_E2E_SOFTWARE" => software,
                "ALEF_E2E_VISIBLE" => visible,
                _ => None,
            }
            .map(str::to_owned)
        };
        assert_eq!(quiet_from(&read), expected_quiet);
        assert_eq!(software_from(&read, windows), expected_software,
            "e2e={e2e:?} quiet={quiet:?} software={software:?} visible={visible:?} windows={windows}");
    };
    for (e2e, quiet, software, visible, windows, expected_quiet, expected_software) in cases {
        check(
            e2e,
            quiet,
            software,
            visible,
            windows,
            expected_quiet,
            expected_software,
        );
    }
    for wrong in ["", "0", "true", "yes", "2", " 1", "1 "] {
        for windows in [false, true] {
            check(
                Some(wrong),
                Some("1"),
                Some("1"),
                None,
                windows,
                false,
                false,
            );
            check(
                Some("1"),
                Some(wrong),
                Some("1"),
                None,
                windows,
                false,
                false,
            );
            check(
                Some("1"),
                Some("1"),
                Some(wrong),
                None,
                windows,
                true,
                windows,
            );
            check(
                Some("1"),
                Some("1"),
                Some("1"),
                Some(wrong),
                windows,
                true,
                true,
            );
            check(
                Some("1"),
                Some("1"),
                Some(wrong),
                Some("1"),
                windows,
                false,
                false,
            );
        }
    }
}

#[cfg(not(feature = "e2e"))]
fn quiet_enabled() -> bool {
    false
}

#[cfg(all(test, not(feature = "e2e")))]
#[test]
fn feature_off_never_enables_quiet_windows() {
    assert!(!quiet_enabled());
}

/// The event loop of a windowed run, with the message hook of the platform menus attached. The
/// menus are given before the loop is built, so their accelerators stay shared with the hook.
fn event_loop(
    menus: &crate::ui::integration::menu::Menus,
) -> Result<EventLoop<Wake>, Box<dyn std::error::Error>> {
    let mut builder = EventLoop::<Wake>::with_user_event();
    #[cfg(target_os = "windows")]
    {
        use windows_sys::Win32::UI::WindowsAndMessaging::{TranslateAcceleratorW, MSG};
        use winit::platform::windows::EventLoopBuilderExtWindows;
        let accelerators = menus.accelerators.clone();
        builder.with_msg_hook(move |raw| {
            // SAFETY: winit passes a live, aligned MSG for the duration of this hook.
            let msg = unsafe { &*(raw as *const MSG) };
            // Snapshot and release RefCell borrow BEFORE potentially reentrant FFI.
            let snapshot = accelerators.borrow().get(&msg.hwnd).cloned();
            let Some(snapshot) = snapshot else {
                return false;
            };
            if snapshot.haccel == 0 {
                return false;
            }
            // SAFETY: UI-thread-only cache; snapshot owns the immutable muda menu
            // and Rc<Window>, keeping HACCEL and HWND live throughout translation.
            // No muda/cache borrow is held across synchronous WM_COMMAND dispatch.
            unsafe { TranslateAcceleratorW(msg.hwnd, snapshot.haccel, msg) != 0 }
        });
    }
    Ok(builder.build()?)
}

/// The application of a windowed run: everything the bridge carries and the options ask for. Used
/// by the cold `run` and, once the assignment arrives, by the warm spare.
pub(in crate::window) fn app_from(
    bridge: &mut Bridge,
    options: WindowOptions,
    menus: crate::ui::integration::menu::Menus,
    proxy: winit::event_loop::EventLoopProxy<Wake>,
    prepared: Option<state::warm::PreparedWindow>,
) -> Result<App, Box<dyn std::error::Error>> {
    let handle = bridge.handle();
    handle.attach(proxy.clone());
    Ok(App {
        url: bridge.entry_url.clone(),
        title: options.title,
        icon_png: options.icon_png,
        app_id: options.app_id,
        icon: None,
        definitions: options.windows,
        registry: RefCell::new(Some(bridge.take_registry()?)),
        waker: Waker(proxy, state::resize_wait::new_generation()),
        windows: Vec::new(),
        started: false,
        quiet: quiet_enabled(),
        next_close: 0,
        error: None,
        handle,
        requests: bridge.take_requests()?,
        sessions: bridge.sessions(),
        ids: bridge.windows(),
        runtime: tokio::runtime::Handle::current(),
        dialogs: host::dialogs::Dialogs::new(),
        shortcuts: crate::ui::integration::shortcut::Shortcuts::default(),
        trays: crate::ui::integration::tray::Trays::default(),
        menus,
        restore: options.state_file.map(state::restore::Restore::load),
        prepared,
        devtools_port: options.devtools_port,
        // Window mode builds its own delegate in `host::manage`; headless keeps one locally.
        bridge_devtools: None,
        servo_started: false,
        profile_dir: options.profile_dir,
        webcrypto_substitute: bridge.webcrypto_substitute(),
    })
}

pub fn run(bridge: &mut Bridge, options: WindowOptions) -> Result<(), Box<dyn std::error::Error>> {
    if options.windows.is_empty() {
        return Err(
            io::Error::new(io::ErrorKind::InvalidInput, "There is no window to open").into(),
        );
    }
    state::resize_wait::set_resize_trace(
        std::env::var("ALEF_RESIZE_TRACE").is_ok_and(|v| v == "1"),
    );
    #[cfg(feature = "e2e")]
    if quiet_enabled() {
        eprintln!("ALEF_E2E quiet: the windows of this run are never shown");
    }
    let menus = crate::ui::integration::menu::Menus::default();
    let event_loop = event_loop(&menus)?;
    let mut app = app_from(bridge, options, menus, event_loop.create_proxy(), None)?;
    event_loop.run_app(&mut app)?;
    if let Some(error) = app.error {
        return Err(error.into());
    }
    Ok(())
}

/// The application a warm spare is assigned to: what `run` would have been given.
pub struct WarmAssignment {
    pub bridge: Bridge,
    pub options: WindowOptions,
}

/// The recipe of a windowed warm spare: a function that waits for the assignment on a worker
/// thread and there runs the whole launch path of the application (the checks, the consent, the
/// environment and working directory of the client, `Bridge::with_options`). It must not touch
/// windows or the event loop: those live on the main thread. The result moves between threads, so
/// every field of [`WarmAssignment`] must be `Send`. An error or a refusal ends the spare without
/// ever showing a window: the client starts cold.
pub struct WarmPrepare {
    pub assignment:
        Box<dyn FnOnce() -> Result<WarmAssignment, Box<dyn std::error::Error + Send>> + Send>,
}

/// The windowed warm spare: the event loop, a hidden window and a GL context exist before the
/// application is known (the most expensive part of the cold start after the document load, see
/// the M11 notes). The assignment runs on a worker thread; when it arrives, the same event loop
/// continues as a usual application whose first window adopts the prepared one. The quiet mode of
/// end-to-end runs (`ALEF_E2E_QUIET`) is read from the environment, as in `run`.
pub fn run_warm(prepare: WarmPrepare) -> Result<(), Box<dyn std::error::Error>> {
    state::resize_wait::set_resize_trace(
        std::env::var("ALEF_RESIZE_TRACE").is_ok_and(|v| v == "1"),
    );
    let quiet = quiet_enabled();
    #[cfg(feature = "e2e")]
    if quiet {
        eprintln!("ALEF_E2E quiet: the windows of this run are never shown");
    }
    let menus = crate::ui::integration::menu::Menus::default();
    let event_loop = event_loop(&menus)?;
    let mut warm = state::warm::warm_app(prepare, menus, quiet, &event_loop);
    event_loop.run_app(&mut warm)?;
    if let Some(error) = warm.error {
        return Err(error);
    }
    if let Some(app) = warm.app {
        if let Some(error) = app.error {
            return Err(error.into());
        }
    }
    Ok(())
}
