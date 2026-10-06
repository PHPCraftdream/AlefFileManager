// SPDX-License-Identifier: MIT OR Apache-2.0
//! Dialog and dialog-verdict support: render-continuity phase tracking, host dialog
//! open/close, and the programmatic WM_CLOSE teardown.
use std::time::{Duration, Instant};

use winit::window::Window;

use super::Cycle;

const DIALOG_DELAY: Duration = Duration::from_millis(800);
const DIALOG_LIFETIME: Duration = Duration::from_secs(4);
const DIALOG_TIMEOUT: Duration = Duration::from_secs(20);

pub(crate) struct Dialog {
    pub(crate) enabled: bool,
    pub(crate) open_at: Option<Instant>,
    pub(crate) found_at: Option<Instant>,
    pub(crate) close_posted_at: Option<Instant>,
    pub(crate) gone_at: Option<Instant>,
}

impl Dialog {
    pub(crate) fn new(enabled: bool, started: Instant) -> Self {
        Self {
            enabled,
            open_at: enabled.then(|| started + DIALOG_DELAY),
            found_at: None,
            close_posted_at: None,
            gone_at: None,
        }
    }
}

/// Next instant the loop must wake for dialog bookkeeping.
pub(crate) fn deadline(dialog: &Dialog) -> Option<Instant> {
    if !dialog.enabled || dialog.gone_at.is_some() {
        return None;
    }
    if dialog.found_at.is_none() {
        return Some(dialog.open_at? + Duration::from_millis(900));
    }
    if dialog.close_posted_at.is_none() {
        return Some(dialog.found_at? + DIALOG_LIFETIME);
    }
    Some(dialog.close_posted_at? + Duration::from_millis(100))
}

pub(crate) fn poll_dialog(
    dialog: &mut Dialog,
    cycle_b: &mut Option<Cycle>,
    started: Instant,
    now: Instant,
) {
    if !dialog.enabled || dialog.gone_at.is_some() {
        return;
    }
    if dialog.found_at.is_none() {
        if let Some(hwnd) = find_dialog_window() {
            dialog.found_at = Some(now);
            super::log(&format!("dialog window visible (hwnd {hwnd:#x})"));
            *cycle_b = Some(Cycle::new(now + DIALOG_LIFETIME + super::CYCLE_B_DELAY));
        } else if now - started >= DIALOG_TIMEOUT {
            super::log("dialog window never became visible: test timed out");
            dialog.gone_at = Some(now);
            if cycle_b.is_none() {
                *cycle_b = Some(Cycle::new(now + super::CYCLE_B_DELAY));
            }
        }
        return;
    }
    let found_at = dialog.found_at.unwrap_or(now);
    if dialog.close_posted_at.is_none()
        && now >= found_at + DIALOG_LIFETIME
        && post_wm_close_to_dialog()
    {
        dialog.close_posted_at = Some(now);
        super::log("posted WM_CLOSE to the dialog window");
    }
    if let Some(posted_at) = dialog.close_posted_at {
        if find_dialog_window().is_none() {
            dialog.gone_at = Some(now);
            super::log(&format!(
                "dialog closed {} ms after WM_CLOSE",
                (now - posted_at).as_millis()
            ));
        }
    }
}

#[cfg(target_os = "windows")]
fn find_dialog_window() -> Option<isize> {
    use windows_sys::Win32::Foundation::HWND;
    use windows_sys::Win32::UI::WindowsAndMessaging::{
        EnumWindows, GetClassNameA, GetWindowThreadProcessId, IsWindowVisible,
    };
    static mut FOUND: Option<isize> = None;
    unsafe extern "system" fn callback(hwnd: HWND, _lparam: isize) -> i32 {
        unsafe {
            let mut pid = 0u32;
            GetWindowThreadProcessId(hwnd, &mut pid);
            if pid == std::process::id() && IsWindowVisible(hwnd) != 0 {
                let mut class = [0u8; 16];
                let len =
                    GetClassNameA(hwnd, class.as_mut_ptr(), class.len() as i32).max(0) as usize;
                if class[..len.min(7)] == *b"#32770" {
                    FOUND = Some(hwnd);
                    return 0;
                }
            }
        }
        1
    }
    unsafe {
        FOUND = None;
        EnumWindows(Some(callback), 0);
        FOUND
    }
}

#[cfg(not(target_os = "windows"))]
fn find_dialog_window() -> Option<isize> {
    None
}

#[cfg(target_os = "windows")]
fn post_wm_close_to_dialog() -> bool {
    use windows_sys::Win32::UI::WindowsAndMessaging::{PostMessageW, WM_CLOSE};
    find_dialog_window().is_some_and(|hwnd| unsafe { PostMessageW(hwnd, WM_CLOSE, 0, 0) != 0 })
}

#[cfg(not(target_os = "windows"))]
fn post_wm_close_to_dialog() -> bool {
    false
}

/// Opens the rfd message dialog on the Tokio runtime (rfd runs the Win32 box on its own
/// thread, so no Tokio worker blocks).
pub(crate) fn open_dialog_async(window: &Window) {
    let dialog = rfd::AsyncMessageDialog::new()
        .set_parent(window)
        .set_title("Alef M0.3 spike")
        .set_description("Modal message dialog opened by the integration spike. It closes itself.")
        .set_buttons(rfd::MessageButtons::Ok)
        .set_level(rfd::MessageLevel::Info);
    let Ok(handle) = tokio::runtime::Handle::try_current() else {
        super::log("no Tokio runtime on this thread: dialog test skipped");
        return;
    };
    handle.spawn(async move {
        tokio::time::sleep(DIALOG_DELAY).await;
        let started = Instant::now();
        let result = dialog.show().await;
        super::log(&format!(
            "rfd dialog returned {result:?} after {:?}",
            started.elapsed()
        ));
    });
}
