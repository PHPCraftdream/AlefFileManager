// SPDX-License-Identifier: MIT OR Apache-2.0
// The scenario: alternating resizes confirmed by observed presents, close order checks,
// verdict computation and the final summary emitted through the page's report route.
use std::cell::Cell;
use std::rc::Rc;
use std::time::{Duration, Instant};

use servo::RenderingContext;
use winit::dpi::LogicalSize;
use winit::event_loop::{ActiveEventLoop, ControlFlow};

use super::verdict::{
    confirm_budget, load_timed_out, step_size, step_target, timeout_tolerance, verdict, Target,
    Verdict, VerdictInput, ALT_STEPS, VERIFY_STEPS,
};
use super::{marker, Pending, Phase, PrimaryView, Spike};
use crate::window::resize_wait::wait_for_generation;

// Pacing of the automated resize steps and settle pauses between phases.
const STEP_INTERVAL: Duration = Duration::from_millis(150);
const SETTLE: Duration = Duration::from_millis(800);

impl Spike {
    pub(crate) fn tick(&mut self, event_loop: &ActiveEventLoop, primary: Option<PrimaryView>) {
        if !self.enabled {
            return;
        }
        if primary.is_some() {
            self.primary = primary;
        }
        if let Some(reason) = self.failure.take() {
            return self.fail(reason);
        }
        if let Some(secondary) = &self.secondary {
            if secondary.crashed.get() > 0 {
                return self.fail("secondary content process crashed");
            }
        }
        if !matches!(self.phase, Phase::Done | Phase::Fail)
            && self.started.elapsed() > Duration::from_secs(150)
        {
            return self.fail("global watchdog timeout");
        }
        let now = Instant::now();
        match self.phase {
            Phase::WaitPrimary => {
                let ready = self
                    .primary
                    .as_ref()
                    .is_some_and(|view| view.page_ready.get());
                if ready {
                    self.w1_loaded = true;
                    let Some(url) = self.url.take() else {
                        return self.fail("entry URL missing");
                    };
                    let Some(view) = &self.primary else {
                        return;
                    };
                    marker("before-secondary");
                    match super::secondary::Secondary::create(
                        &view.servo,
                        event_loop,
                        url,
                        view.wake_gen.clone(),
                        self.trace,
                    ) {
                        Ok(secondary) => {
                            let id = format!("{:?}", secondary.window.id());
                            let size = secondary.window.inner_size();
                            self.log(format!(
                                "secondary window created id={id} size={}x{}",
                                size.width, size.height
                            ));
                            self.secondary = Some(secondary);
                            self.phase = Phase::WaitSecondary;
                            self.deadline = now + Duration::from_secs(140);
                        }
                        Err(error) => self.fail(format!("secondary creation failed: {error}")),
                    }
                } else if load_timed_out(now, self.deadline) {
                    self.fail("primary page did not load within 120 s");
                }
            }
            Phase::WaitSecondary => {
                let ready = self
                    .secondary
                    .as_ref()
                    .is_some_and(|s| s.page_ready.get() && s.frame_ready.get());
                if ready {
                    self.w2_loaded = true;
                    marker("secondary-loaded");
                    eprintln!("MW phase=AlternateResize begin steps={ALT_STEPS}");
                    self.phase = Phase::AlternateResize;
                    self.step = 0;
                    self.next_step = now + Duration::from_millis(300);
                    self.deadline = now + Duration::from_secs(120);
                } else if load_timed_out(now, self.deadline) {
                    self.fail("secondary page did not load in time");
                }
            }
            Phase::AlternateResize => {
                if self.secondary_close_requested {
                    self.close_secondary("close requested mid-sequence");
                }
                if self.secondary.is_none() {
                    self.fail("secondary window disappeared mid-sequence");
                } else if self.pending.is_some() {
                    self.check_pending(now);
                } else if self.step < ALT_STEPS {
                    if now >= self.next_step {
                        self.issue_step(self.step, false, now);
                    }
                } else if now >= self.next_step + SETTLE {
                    marker("before-secondary-close");
                    self.close_secondary("sequence complete");
                    eprintln!("MW phase=VerifyPrimary begin steps={VERIFY_STEPS}");
                    self.phase = Phase::VerifyPrimary;
                    self.step = 0;
                    self.settled = false;
                    self.next_step = now + Duration::from_millis(1500);
                    self.deadline = now + Duration::from_secs(60);
                }
                if matches!(self.phase, Phase::AlternateResize) && now >= self.deadline {
                    self.fail("alternating resize sequence stalled");
                }
            }
            Phase::VerifyPrimary => {
                if self.secondary_close_requested {
                    self.close_secondary("close requested");
                }
                if !self.settled {
                    if now >= self.next_step {
                        marker("after-secondary-close");
                        self.settled = true;
                        self.next_step = now + Duration::from_millis(200);
                    }
                } else if self.pending.is_some() {
                    self.check_pending(now);
                } else if self.step < VERIFY_STEPS {
                    if now >= self.next_step {
                        self.issue_step(self.step, true, now);
                    }
                } else if now >= self.next_step + SETTLE {
                    marker("before-primary-close");
                    self.phase = Phase::ClosePrimary;
                    self.next_step = now + Duration::from_millis(50);
                }
                if matches!(self.phase, Phase::VerifyPrimary) && now >= self.deadline {
                    self.fail("primary verification stalled");
                }
            }
            Phase::ClosePrimary => {
                let checks = self.verdict_checks(true);
                self.finish(checks, event_loop);
            }
            Phase::Fail => {
                let checks = self.verdict_checks(false);
                self.finish(checks, event_loop);
            }
            Phase::Done => {}
        }
        self.pace(event_loop);
    }

    /// Issue a resize step; the target window must exist or the scenario fails.
    fn issue_step(&mut self, step: usize, verify: bool, now: Instant) {
        let target = step_target(step, verify);
        let (width, height) = step_size(step, verify);
        let budget = now + confirm_budget(self.strict);
        match target {
            Target::Primary => {
                let Some(view) = &self.primary else {
                    return self.fail("resize step targeted a missing primary window");
                };
                let physical =
                    LogicalSize::new(width, height).to_physical::<u32>(view.window.scale_factor());
                let _ = view
                    .window
                    .request_inner_size(LogicalSize::new(width, height));
                self.pending = Some(Pending {
                    step,
                    verify,
                    target,
                    size: (physical.width, physical.height),
                    budget,
                    presents_at_issue: view.presents.get(),
                });
            }
            Target::Secondary => {
                let Some(secondary) = &self.secondary else {
                    return self.fail("resize step targeted a missing secondary window");
                };
                let physical = LogicalSize::new(width, height)
                    .to_physical::<u32>(secondary.window.scale_factor());
                let _ = secondary
                    .window
                    .request_inner_size(LogicalSize::new(width, height));
                let presents_at_issue = secondary.presents;
                self.pending = Some(Pending {
                    step,
                    verify,
                    target,
                    size: (physical.width, physical.height),
                    budget,
                    presents_at_issue,
                });
            }
            Target::Unknown => return self.fail("resize step with unknown target"),
        }
        self.requested += 1;
        let owner = if target == Target::Primary {
            "primary"
        } else {
            "secondary"
        };
        let kind = if verify { "verify" } else { "alt" };
        self.log(format!(
            "step {kind}={} win={owner} size={width}x{height}",
            step + 1
        ));
    }

    /// Confirm a pending step by a present at the requested size observed within the budget; a late or missing present is a timeout.
    fn check_pending(&mut self, now: Instant) {
        let Some(pending) = &self.pending else {
            return;
        };
        let confirmed = match pending.target {
            Target::Primary => self.primary.as_ref().is_some_and(|view| {
                let size = view.rendering.size();
                (size.width, size.height) == pending.size
                    && view.presents.get() > pending.presents_at_issue
            }),
            Target::Secondary => self.secondary.as_ref().is_some_and(|secondary| {
                secondary.last_present_size() == pending.size
                    && secondary.presents > pending.presents_at_issue
            }),
            Target::Unknown => false,
        };
        if confirmed && now < pending.budget {
            let pending = self.pending.take().expect("pending step");
            let owner = if pending.target == Target::Primary {
                "primary"
            } else {
                "secondary"
            };
            let kind = if pending.verify { "verify" } else { "alt" };
            self.confirmed += 1;
            if pending.verify {
                self.verify_confirmed += 1;
            }
            self.log(format!(
                "{kind} step {} confirmed for {owner}",
                pending.step + 1
            ));
        } else if now < pending.budget {
            return; // still waiting for the confirming present
        } else {
            // Budget expired: the present is late or never happened — a timeout either way.
            let pending = self.pending.take().expect("pending step");
            let owner = if pending.target == Target::Primary {
                "primary"
            } else {
                "secondary"
            };
            let kind = if pending.verify { "verify" } else { "alt" };
            self.timeouts += 1;
            self.log(format!(
                "{kind} step {} for {owner} NOT confirmed at {}x{} within budget",
                pending.step + 1,
                pending.size.0,
                pending.size.1
            ));
        }
        self.step += 1;
        self.next_step = now + STEP_INTERVAL;
    }

    // Emit the verdict, mark failure, and leave the event loop.
    fn finish(&mut self, checks: Verdict, event_loop: &ActiveEventLoop) {
        self.failed = !checks.passed;
        self.log_summary(&checks);
        self.emit_summary(&checks);
        self.phase = Phase::Done;
        event_loop.exit();
    }

    fn fail(&mut self, reason: impl Into<String>) {
        if matches!(self.phase, Phase::Fail | Phase::Done) {
            return;
        }
        let reason = reason.into();
        self.log(format!("FAIL reason={reason}"));
        marker("fail");
        self.close_secondary("failure");
        self.next_step = Instant::now();
        self.phase = Phase::Fail;
    }

    pub(crate) fn close_secondary(&mut self, reason: &str) {
        let Some(secondary) = self.secondary.take() else {
            self.secondary_close_requested = false;
            return;
        };
        let id = format!("{:?}", secondary.window.id());
        let (presents, timeouts) = (secondary.presents, secondary.timeouts);
        let crashes = secondary.crashed.get();
        // Drop order: webview (painter teardown), rendering context, native window.
        drop(secondary);
        self.w2_presents += presents;
        self.w2_timeouts += timeouts;
        self.w2_crashes += crashes;
        self.secondary_close_requested = false;
        self.log(format!(
            "secondary window {id} closed ({reason}) presents={presents} timeouts={timeouts}"
        ));
    }

    fn verdict_checks(&self, clean_exit: bool) -> Verdict {
        verdict(VerdictInput {
            w1_loaded: self.w1_loaded,
            w2_loaded: self.w2_loaded,
            requested: self.requested,
            confirmed: self.confirmed,
            timeouts: self.timeouts + self.w2_timeouts,
            timeout_tolerance: timeout_tolerance(self.strict),
            crashes: self
                .secondary
                .as_ref()
                .map_or(0, |secondary| secondary.crashed.get())
                + self.w2_crashes,
            w2_closed_w1_alive: self.verify_confirmed == VERIFY_STEPS as u32,
            clean_exit,
        })
    }

    // Rust-side summary first, then the page POSTs the machine-readable mw-summary so the
    // `spike report:` line is the last one on stderr.
    fn emit_summary(&mut self, checks: &Verdict) {
        let Some(view) = &self.primary else {
            return;
        };
        let payload = serde_json::to_string(checks).expect("verdict JSON");
        let done = Rc::new(Cell::new(false));
        let flag = done.clone();
        view.webview.evaluate_javascript(
            format!("window.__alefMwSummary && window.__alefMwSummary({payload});"),
            move |_| flag.set(true),
        );
        let deadline = Instant::now() + Duration::from_secs(8);
        loop {
            let seen = *view.wake_gen.0.lock().unwrap();
            view.servo.spin_event_loop();
            if done.get() || Instant::now() >= deadline {
                break;
            }
            wait_for_generation(&view.wake_gen, seen, deadline);
        }
        // Let the page's report POST reach stderr before the process exits.
        let until = Instant::now() + Duration::from_millis(1500);
        while Instant::now() < until {
            view.servo.spin_event_loop();
            std::thread::sleep(Duration::from_millis(50));
        }
    }

    fn log_summary(&self, checks: &Verdict) {
        eprintln!(
            "MW summary elapsed={:.3}s failed={} requested={} confirmed={} timeouts={} w2_presents={} w2_timeouts={}",
            self.started.elapsed().as_secs_f64(),
            !checks.passed,
            self.requested,
            self.confirmed,
            self.timeouts + self.w2_timeouts,
            self.w2_presents,
            self.w2_timeouts
        );
    }

    // Wake the loop for the next automated step or the secondary's animation frame,
    // never later than a deadline the host already set (minimum deadline only).
    fn pace(&self, event_loop: &ActiveEventLoop) {
        let mut wake = self.next_wake();
        if let Some(secondary) = &self.secondary {
            if secondary.animating.get() {
                wake = Some(match wake {
                    Some(at) if at < secondary.next_frame => at,
                    _ => secondary.next_frame,
                });
            }
        }
        let Some(at) = wake else {
            return;
        };
        match event_loop.control_flow() {
            ControlFlow::Wait => event_loop.set_control_flow(ControlFlow::WaitUntil(at)),
            ControlFlow::WaitUntil(current) if current > at => {
                event_loop.set_control_flow(ControlFlow::WaitUntil(at));
            }
            _ => {}
        }
    }

    // Every non-terminal phase must wake the loop on its own; the host may sleep otherwise.
    fn next_wake(&self) -> Option<Instant> {
        match self.phase {
            Phase::Done => None,
            Phase::WaitPrimary | Phase::WaitSecondary => Some(self.deadline),
            Phase::AlternateResize if self.pending.is_some() => {
                Some(self.pending.as_ref().expect("pending").budget)
            }
            Phase::AlternateResize if self.step < ALT_STEPS => Some(self.next_step),
            Phase::AlternateResize => Some(self.next_step + SETTLE),
            Phase::VerifyPrimary if !self.settled || self.step < VERIFY_STEPS => {
                Some(self.next_step)
            }
            Phase::VerifyPrimary => Some(self.next_step + SETTLE),
            Phase::ClosePrimary | Phase::Fail => Some(self.next_step),
        }
    }

    #[cfg(test)]
    pub(in crate::spikes::multiwindow) fn next_wake_via_test(&self) -> Option<Instant> {
        self.next_wake()
    }
}
