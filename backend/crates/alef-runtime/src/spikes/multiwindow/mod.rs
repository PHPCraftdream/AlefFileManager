// SPDX-License-Identifier: MIT OR Apache-2.0
// M0.2 multi-window spike (docs/stages/m0-spikes.md), enabled by ALEF_SPIKE_MULTIWINDOW=1.
// Second winit window with its own WindowRenderingContext and WebView on the same Servo
// instance; automated resize/close scenario that judges itself and fails the process
// (non-zero exit via the host error path) when any check fails.
// ALEF_SPIKE_MW_STRICT=1: zero timeout tolerance + 1 ms confirmation budget; used only to
// prove the oracle reports failure (documented, deterministic FAIL).
mod scenario;
mod secondary;
#[cfg(test)]
mod tests;
mod verdict;

use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use servo::{Servo, WebView, WindowRenderingContext};
use url::Url;
use winit::event::WindowEvent;
use winit::window::{Window, WindowId};

use crate::window::resize_wait::WakeGeneration;
use secondary::Secondary;
use verdict::Target;

/// Parts of the primary window's `State` the spike needs, cloned out in the hook.
pub(crate) struct PrimaryView {
    pub(crate) servo: Servo,
    pub(crate) webview: WebView,
    pub(crate) rendering: Rc<WindowRenderingContext>,
    pub(crate) window: Rc<Window>,
    pub(crate) page_ready: Rc<Cell<bool>>,
    /// Present counter incremented by the host on every paint+present (state.rs trace_present).
    pub(crate) presents: Rc<Cell<u32>>,
    pub(crate) wake_gen: WakeGeneration,
}

#[derive(Clone, Copy, PartialEq)]
enum Phase {
    WaitPrimary,
    WaitSecondary,
    AlternateResize,
    VerifyPrimary,
    ClosePrimary,
    Fail,
    Done,
}

/// A resize step issued but not yet confirmed by a present at the requested size.
struct Pending {
    step: usize,
    verify: bool,
    target: Target,
    size: (u32, u32),
    budget: Instant,
    presents_at_issue: u32,
}

pub(crate) struct Spike {
    enabled: bool,
    trace: bool,
    /// Strict oracle: guaranteed failure mode used to prove the oracle can fail.
    strict: bool,
    url: Option<Url>,
    primary: Option<PrimaryView>,
    secondary: Option<Secondary>,
    secondary_close_requested: bool,
    phase: Phase,
    failure: Option<String>,
    failed: bool,
    w1_loaded: bool,
    w2_loaded: bool,
    started: Instant,
    next_step: Instant,
    deadline: Instant,
    settled: bool,
    step: usize,
    requested: u32,
    confirmed: u32,
    timeouts: u32,
    verify_confirmed: u32,
    w2_presents: u32,
    w2_timeouts: u32,
    w2_crashes: u32,
    pending: Option<Pending>,
}

impl Spike {
    pub(crate) fn new() -> Self {
        let one = |name| std::env::var(name).is_ok_and(|value| value == "1");
        Self {
            enabled: one("ALEF_SPIKE_MULTIWINDOW"),
            trace: one("ALEF_RESIZE_TRACE"),
            strict: one("ALEF_SPIKE_MW_STRICT"),
            url: None,
            primary: None,
            secondary: None,
            secondary_close_requested: false,
            phase: Phase::WaitPrimary,
            failure: None,
            failed: false,
            w1_loaded: false,
            w2_loaded: false,
            started: Instant::now(),
            next_step: Instant::now(),
            deadline: Instant::now(),
            settled: false,
            step: 0,
            requested: 0,
            confirmed: 0,
            timeouts: 0,
            verify_confirmed: 0,
            w2_presents: 0,
            w2_timeouts: 0,
            w2_crashes: 0,
            pending: None,
        }
    }

    pub(crate) fn enabled(&self) -> bool {
        self.enabled
    }

    pub(crate) fn resumed(&mut self, url: Url) {
        if !self.enabled {
            return;
        }
        self.url = Some(url);
        self.deadline = Instant::now() + Duration::from_secs(120);
        self.log("armed");
    }

    /// Routes events of spike-owned windows; `true` means the event was consumed.
    pub(crate) fn window_event(&mut self, window_id: WindowId, event: &WindowEvent) -> bool {
        if !self.enabled {
            return false;
        }
        let secondary_id = self
            .secondary
            .as_ref()
            .map(|secondary| secondary.window.id());
        let owned = verdict::route(window_id, None, secondary_id) == Target::Secondary;
        if !owned {
            return false;
        }
        if matches!(event, WindowEvent::CloseRequested) {
            self.secondary_close_requested = true;
            self.log("secondary close requested (consumed; the spike owns this window)");
            return true;
        }
        let outcome = match &mut self.secondary {
            Some(secondary) => secondary.on_event(event),
            None => return true,
        };
        if let Err(error) = outcome {
            self.failure = Some(format!("secondary window failed: {error}"));
        }
        true
    }

    /// Error for the host's exit path when the oracle failed (non-zero process exit).
    pub(crate) fn exit_error(&mut self) -> Option<String> {
        if self.failed && matches!(self.phase, Phase::Done) {
            Some("M0.2 multiwindow oracle failed; see the MW lines in stderr".to_string())
        } else {
            None
        }
    }

    // Runs while the event loop is still alive; the runtime state may already be dropped.
    pub(crate) fn shutdown(&mut self) {
        if self.secondary.is_some() {
            self.close_secondary("event loop exiting");
        }
    }

    pub(crate) fn log(&self, message: impl std::fmt::Display) {
        eprintln!("MW t={:.3} {message}", self.started.elapsed().as_secs_f64());
    }
}

pub(crate) fn marker(name: &str) {
    let unix_ms = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |value| value.as_millis());
    eprintln!(
        "MW marker name={name} unix_ms={unix_ms} pid={}",
        std::process::id()
    );
}
