// SPDX-License-Identifier: MIT OR Apache-2.0
// Pure scenario logic: routing, step plan, deadlines and the pass/fail verdict.
use serde::Serialize;
use std::time::{Duration, Instant};

/// Which window an event or scenario step belongs to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(in crate::spikes::multiwindow) enum Target {
    Primary,
    Secondary,
    Unknown,
}

/// Secondary wins when both match (the spike owns it); otherwise primary; else unknown.
pub(in crate::spikes::multiwindow) fn route<T: Copy + PartialEq>(
    id: T,
    primary: Option<T>,
    secondary: Option<T>,
) -> Target {
    if secondary == Some(id) {
        Target::Secondary
    } else if primary == Some(id) {
        Target::Primary
    } else {
        Target::Unknown
    }
}

pub(in crate::spikes::multiwindow) const ALT_STEPS: usize = 30;
pub(in crate::spikes::multiwindow) const VERIFY_STEPS: usize = 5;
const ALT: [(f64, f64); 5] = [
    (940.0, 690.0),
    (1200.0, 800.0),
    (1040.0, 640.0),
    (1250.0, 760.0),
    (880.0, 700.0),
];
const VERIFY: [(f64, f64); 5] = [
    (1100.0, 700.0),
    (1280.0, 780.0),
    (980.0, 660.0),
    (1300.0, 800.0),
    (1200.0, 800.0),
];

/// Logical size of a step: `verify` steps belong to the primary-verification phase.
pub(in crate::spikes::multiwindow) fn step_size(step: usize, verify: bool) -> (f64, f64) {
    let sizes = if verify { &VERIFY } else { &ALT };
    sizes[step % sizes.len()]
}

/// Window a scenario step resizes; even alt steps hit the primary, odd the secondary.
pub(in crate::spikes::multiwindow) fn step_target(step: usize, verify: bool) -> Target {
    if verify || step.is_multiple_of(2) {
        Target::Primary
    } else {
        Target::Secondary
    }
}

/// Whether a page-load phase missed its deadline.
pub(in crate::spikes::multiwindow) fn load_timed_out(now: Instant, deadline: Instant) -> bool {
    now >= deadline
}

/// Tolerance: one timeout per full run (35 steps). The first step after page load can miss
/// the 100 ms frame budget on a loaded machine (observed once); anything more is a failure.
pub(in crate::spikes::multiwindow) fn timeout_tolerance(strict: bool) -> u32 {
    if strict {
        0
    } else {
        1
    }
}

/// Budget for confirming a resize by observing a present; strict mode makes it impossible
/// on purpose to prove the oracle can fail.
pub(in crate::spikes::multiwindow) fn confirm_budget(strict: bool) -> Duration {
    if strict {
        Duration::from_millis(1)
    } else {
        Duration::from_millis(2000)
    }
}

/// Everything the verdict is computed from.
#[derive(Clone, Copy)]
pub(in crate::spikes::multiwindow) struct VerdictInput {
    pub w1_loaded: bool,
    pub w2_loaded: bool,
    pub requested: u32,
    pub confirmed: u32,
    pub timeouts: u32,
    pub timeout_tolerance: u32,
    pub crashes: u32,
    pub w2_closed_w1_alive: bool,
    pub clean_exit: bool,
}

/// Named checks reported to the page (serde field order is the JSON key order).
#[derive(Debug, PartialEq, Serialize)]
pub(in crate::spikes::multiwindow) struct Verdict {
    pub w1_loaded: bool,
    pub w2_loaded: bool,
    pub resizes_confirmed: bool,
    pub timeouts_within_tolerance: bool,
    pub no_crash: bool,
    pub w2_closed_w1_alive: bool,
    pub clean_exit: bool,
    #[serde(skip)]
    pub passed: bool,
}

pub(in crate::spikes::multiwindow) fn verdict(input: VerdictInput) -> Verdict {
    let resizes_confirmed = input.requested > 0 && input.confirmed == input.requested;
    let timeouts_within_tolerance = input.timeouts <= input.timeout_tolerance;
    let no_crash = input.crashes == 0;
    let passed = input.w1_loaded
        && input.w2_loaded
        && resizes_confirmed
        && timeouts_within_tolerance
        && no_crash
        && input.w2_closed_w1_alive
        && input.clean_exit;
    Verdict {
        w1_loaded: input.w1_loaded,
        w2_loaded: input.w2_loaded,
        resizes_confirmed,
        timeouts_within_tolerance,
        no_crash,
        w2_closed_w1_alive: input.w2_closed_w1_alive,
        clean_exit: input.clean_exit,
        passed,
    }
}
