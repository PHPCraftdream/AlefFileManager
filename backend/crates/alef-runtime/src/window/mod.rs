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
}

impl WindowOptions {
    /// One window, 1200x800, centred, with the native frame.
    pub fn new(title: impl Into<String>, icon_png: Vec<u8>) -> Self {
        Self {
            title: title.into(),
            icon_png,
            state_file: None,
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

pub fn run(bridge: &mut Bridge, options: WindowOptions) -> Result<(), Box<dyn std::error::Error>> {
    if options.windows.is_empty() {
        return Err(
            io::Error::new(io::ErrorKind::InvalidInput, "There is no window to open").into(),
        );
    }
    state::resize_wait::set_resize_trace(
        std::env::var("ALEF_RESIZE_TRACE").is_ok_and(|v| v == "1"),
    );
    let quiet = quiet_enabled();
    #[cfg(feature = "e2e")]
    if quiet {
        eprintln!("ALEF_E2E quiet: the windows of this run are never shown");
    }
    let menus = crate::ui::integration::menu::Menus::default();
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
    let event_loop = builder.build()?;
    let handle = bridge.handle();
    handle.attach(event_loop.create_proxy());
    let mut app = App {
        url: bridge.entry_url.clone(),
        title: options.title,
        icon_png: options.icon_png,
        icon: None,
        definitions: options.windows,
        registry: RefCell::new(Some(bridge.take_registry()?)),
        waker: Waker(
            event_loop.create_proxy(),
            state::resize_wait::new_generation(),
        ),
        windows: Vec::new(),
        started: false,
        quiet,
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
    };
    event_loop.run_app(&mut app)?;
    if let Some(error) = app.error {
        return Err(error.into());
    }
    Ok(())
}
