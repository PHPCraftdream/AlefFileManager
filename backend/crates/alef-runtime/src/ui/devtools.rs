// SPDX-License-Identifier: MIT OR Apache-2.0
//! Main-thread Servo delegate for DevTools consent (M10.2).
//!
//! `AllowOrDenyRequest` wraps an IPC responder and is not `Send`: it never leaves the Servo
//! owner thread. It rests in a single pending slot; only a plain boolean answer produced by
//! the async `rfd` dialog on the Tokio runtime crosses back, and `poll` (called once per
//! event-loop tick on the owner thread) turns it into `allow`/`deny`. A second request while a
//! dialog is open is denied at once; a closed or timed-out dialog is a denial.
//!
//! In end-to-end builds (`feature = "e2e"`) no dialog is ever shown: every request is denied,
//! and `notify_devtools_server_started` prints the `ALEF_E2E devtools port=<port> token=<token>`
//! line the acceptance check reads. In normal builds the token is neither printed nor kept.
//!
//! The rules that decide are the pure functions `admission` (is the user asked at all?) and, in
//! normal builds, `dialog_consent` (which dialog answer consents): both are tested without
//! Servo and without a dialog.
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::mpsc,
};

#[cfg(not(feature = "e2e"))]
use rfd::{AsyncMessageDialog, MessageButtons, MessageDialogResult, MessageLevel};
use servo::{AllowOrDenyRequest, ServoDelegate, ServoError};

/// How long a confirmation dialog may stay open; expiry denies the request.
#[cfg(not(feature = "e2e"))]
const DIALOG_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(120);

/// The label of the one button that consents; the other button, and closing the dialog, deny.
#[cfg(not(feature = "e2e"))]
const ALLOW_LABEL: &str = "Allow";

/// What the delegate does with a request before anybody is asked.
#[derive(Debug, PartialEq, Eq, Clone, Copy)]
pub(crate) enum Admission {
    /// Answer deny at once, without showing anything.
    Deny,
    /// Put the question to the user.
    Ask,
}

/// An end-to-end build never asks anybody, and a request that arrives while a dialog is
/// already open is turned away: one dialog at a time.
pub(crate) fn admission(e2e_build: bool, dialog_busy: bool) -> Admission {
    if e2e_build || dialog_busy {
        Admission::Deny
    } else {
        Admission::Ask
    }
}

/// Only the exact label of the consenting button consents. Closing the dialog (`Cancel`), the
/// other button and any other result are denials. Where rfd cannot show custom labels (Windows
/// without the common controls v6 manifest) the answer carries no label and nothing consents:
/// the failure is a denial, never an approval.
#[cfg(not(feature = "e2e"))]
pub(crate) fn dialog_consent(result: &MessageDialogResult) -> bool {
    matches!(result, MessageDialogResult::Custom(label) if label == ALLOW_LABEL)
}

/// The diagnostic a host prints (and exits with) when the DevTools server cannot start.
pub(crate) fn unavailable_message(port: u16) -> String {
    format!("DEVTOOLS_UNAVAILABLE: port {port} is not available")
}

pub(crate) struct DevtoolsDelegate {
    /// The single not-`Send` responder; `None` unless a dialog is open.
    pending: RefCell<Option<AllowOrDenyRequest>>,
    /// The answer channel of the open dialog, if any.
    response: RefCell<Option<mpsc::Receiver<bool>>>,
    /// Wakes the winit event loop from the Tokio task; `Send + Sync` by design.
    wake: std::sync::Arc<dyn Fn() + Send + Sync>,
    /// The runtime that shows the dialog; `None` denies every request.
    #[cfg_attr(feature = "e2e", allow(dead_code))]
    runtime: Option<tokio::runtime::Handle>,
    failed: Rc<Cell<bool>>,
    started: Cell<bool>,
    /// The application the page belongs to, named in the consent dialog.
    #[cfg_attr(feature = "e2e", allow(dead_code))]
    app_name: String,
    /// The package identity of the application, for the same dialog.
    #[cfg_attr(feature = "e2e", allow(dead_code))]
    app_id: String,
}

impl DevtoolsDelegate {
    pub(crate) fn new(
        wake: impl Fn() + Send + Sync + 'static,
        failed: Rc<Cell<bool>>,
        app_name: String,
        app_id: String,
    ) -> Rc<Self> {
        Rc::new(Self {
            pending: RefCell::new(None),
            response: RefCell::new(None),
            wake: std::sync::Arc::new(wake),
            runtime: tokio::runtime::Handle::try_current().ok(),
            failed,
            started: Cell::new(false),
            app_name,
            app_id,
        })
    }

    pub(crate) fn failed(&self) -> bool {
        self.failed.get()
    }

    pub(crate) fn server_started(&self) -> bool {
        self.started.get()
    }

    /// Called on Servo's owner thread once per event-loop tick. Never waits for the dialog.
    pub(crate) fn poll(&self) {
        let answer =
            self.response
                .borrow()
                .as_ref()
                .and_then(|receiver| match receiver.try_recv() {
                    Ok(answer) => Some(answer),
                    Err(mpsc::TryRecvError::Empty) => None,
                    // The dialog task is gone (runtime shutdown): deny.
                    Err(mpsc::TryRecvError::Disconnected) => Some(false),
                });
        if let Some(answer) = answer {
            self.response.borrow_mut().take();
            if let Some(request) = self.pending.borrow_mut().take() {
                if answer {
                    request.allow();
                } else {
                    request.deny();
                }
            }
        }
    }

    /// Opens the confirmation dialog for `request`; denies it when no runtime can show one.
    #[cfg(not(feature = "e2e"))]
    fn ask(&self, request: AllowOrDenyRequest) {
        let Some(runtime) = self.runtime.clone() else {
            request.deny();
            return;
        };
        let description = format!(
            "A debugger on this computer asks to connect to the page of {:?} ({}). \
             A debugger can run any JavaScript in the page with the abilities of this \
             application. Allow it?",
            self.app_name, self.app_id
        );
        // The first custom button is the platform default (rfd 0.17: Windows `nDefaultButton:
        // 0`, macOS the first button added), so Deny goes first; closing the dialog is `Cancel`.
        let dialog = AsyncMessageDialog::new()
            .set_title("DevTools connection")
            .set_description(&description)
            .set_level(MessageLevel::Warning)
            .set_buttons(MessageButtons::OkCancelCustom(
                "Deny".to_owned(),
                ALLOW_LABEL.to_owned(),
            ));
        let (sender, receiver) = mpsc::sync_channel(1);
        *self.pending.borrow_mut() = Some(request);
        *self.response.borrow_mut() = Some(receiver);
        let wake = self.wake.clone();
        runtime.spawn(async move {
            let consent = match tokio::time::timeout(DIALOG_TIMEOUT, dialog.show()).await {
                Ok(result) => dialog_consent(&result),
                Err(_) => false,
            };
            let _ = sender.send(consent);
            wake();
        });
    }

    /// An end-to-end build never shows a dialog: `admission` never lets a request get here.
    #[cfg(feature = "e2e")]
    fn ask(&self, request: AllowOrDenyRequest) {
        request.deny();
    }
}

impl ServoDelegate for DevtoolsDelegate {
    fn notify_devtools_server_started(&self, port: u16, token: String) {
        #[cfg(feature = "e2e")]
        eprintln!("ALEF_E2E devtools port={port} token={token}");
        #[cfg(not(feature = "e2e"))]
        // The token bypasses the consent dialog: in normal builds it is dropped, never
        // logged and never retained.
        let _ = (port, token);
        self.started.set(true);
        (self.wake)();
    }

    fn notify_error(&self, error: ServoError) {
        if matches!(error, ServoError::DevtoolsFailedToStart) {
            // No process::exit here: the callback must not skip cleanup. The flag makes the
            // next event-loop tick route the exit (code 1) on the host side.
            self.failed.set(true);
            (self.wake)();
        }
    }

    fn request_devtools_connection(&self, request: AllowOrDenyRequest) {
        let busy = self.pending.borrow().is_some();
        match admission(cfg!(feature = "e2e"), busy) {
            Admission::Deny => request.deny(),
            Admission::Ask => self.ask(request),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_end_to_end_build_and_a_busy_dialog_deny_and_only_a_free_ordinary_build_asks() {
        assert_eq!(admission(false, false), Admission::Ask);
        assert_eq!(admission(true, false), Admission::Deny);
        assert_eq!(admission(false, true), Admission::Deny);
        assert_eq!(admission(true, true), Admission::Deny);
    }

    #[test]
    fn the_unavailable_diagnostic_names_the_port_exactly() {
        assert_eq!(
            unavailable_message(7000),
            "DEVTOOLS_UNAVAILABLE: port 7000 is not available"
        );
    }

    #[test]
    fn the_failure_flag_starts_clear_and_stays_set_once_raised() {
        let failed = Rc::new(Cell::new(false));
        let delegate =
            DevtoolsDelegate::new(|| {}, failed.clone(), "App".to_owned(), String::new());
        assert!(!delegate.failed());
        delegate.notify_error(ServoError::DevtoolsFailedToStart);
        assert!(delegate.failed());
        assert!(failed.get());
        delegate.notify_error(ServoError::DevtoolsFailedToStart);
        assert!(delegate.failed());
    }

    #[test]
    fn a_started_server_is_remembered_and_wakes_the_loop() {
        let woken = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let seen = woken.clone();
        let delegate = DevtoolsDelegate::new(
            move || {
                seen.fetch_add(1, std::sync::atomic::Ordering::SeqCst);
            },
            Rc::new(Cell::new(false)),
            "App".to_owned(),
            String::new(),
        );
        assert!(!delegate.server_started());
        delegate.notify_devtools_server_started(7000, "TOKEN".to_owned());
        assert!(delegate.server_started());
        assert_eq!(woken.load(std::sync::atomic::Ordering::SeqCst), 1);
    }
}

#[cfg(all(test, not(feature = "e2e")))]
mod dialog_tests {
    use super::*;

    #[test]
    fn only_the_exact_allow_button_consents() {
        let custom = |label: &str| MessageDialogResult::Custom(label.to_owned());
        assert!(dialog_consent(&custom("Allow")));
        for denied in [
            custom("Deny"),
            custom("allow"),
            custom("Allow "),
            custom(""),
            MessageDialogResult::Cancel,
            MessageDialogResult::Ok,
            MessageDialogResult::Yes,
            MessageDialogResult::No,
        ] {
            assert!(!dialog_consent(&denied), "{denied:?} must deny");
        }
    }
}
