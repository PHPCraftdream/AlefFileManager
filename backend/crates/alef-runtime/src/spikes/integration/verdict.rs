// SPDX-License-Identifier: MIT OR Apache-2.0
//! Verdict bookkeeping: per-check booleans, exact probe-sequence tracking, counters,
//! and the final JSON verdict. `null` means "not attempted" and never fails a run.
use super::forward::PROBE_EVENTS;

pub(crate) const DIALOG_CONTINUITY_MIN: f64 = 0.35;

#[derive(Default)]
pub(crate) struct Cleanup {
    pub(crate) tray: Option<bool>,
    pub(crate) menu: Option<bool>,
    pub(crate) hotkey: Option<bool>,
}

#[derive(Default)]
pub(crate) struct Verdict {
    pub(crate) created_menu: Option<bool>,
    pub(crate) created_tray: Option<bool>,
    pub(crate) created_hotkey: Option<bool>,
    pub(crate) tray_events: u32,
    pub(crate) menu_events: u32,
    pub(crate) hotkey_events: u32,
    pub(crate) probe_next: u32,
    pub(crate) probe_broken: bool,
    pub(crate) resize_timeouts: u32,
    pub(crate) resize_requested: u32,
    pub(crate) presents_baseline: u32,
    pub(crate) presents_during: u32,
    pub(crate) cleanup: Cleanup,
}

impl Verdict {
    /// Strict in-order sequence: index must equal the expected next value; any gap,
    /// loss or duplicate marks the sequence broken and the forwarding check fails.
    pub(crate) fn observe_probe(&mut self, index: u32) {
        if index == self.probe_next {
            self.probe_next += 1;
        } else {
            self.probe_broken = true;
        }
    }

    pub(crate) fn forwarding_ok(&self, expected: u32) -> bool {
        !self.probe_broken && self.probe_next == expected
    }

    pub(crate) fn note_present(&mut self, timed_out: bool, during_dialog: bool) {
        if timed_out {
            self.resize_timeouts += 1;
        }
        if during_dialog {
            self.presents_during += 1;
        } else {
            self.presents_baseline += 1;
        }
    }

    fn flag(value: Option<bool>) -> &'static str {
        match value {
            Some(true) => "true",
            Some(false) => "false",
            None => "null",
        }
    }

    fn created(&self) -> Option<bool> {
        Some(self.created_menu? && self.created_tray? && self.created_hotkey?)
    }

    /// One summary line with all counters, printed when the resize cycles finish.
    pub(crate) fn summary(&self, started: std::time::Instant, now: std::time::Instant) -> String {
        format!(
            "summary elapsed={:?} tray={} menu={} hotkey={} probes_next={} probe_broken={} resize_timeouts={} presents_baseline={} presents_during={}",
            now - started,
            self.tray_events,
            self.menu_events,
            self.hotkey_events,
            self.probe_next,
            self.probe_broken,
            self.resize_timeouts,
            self.presents_baseline,
            self.presents_during,
        )
    }

    /// Computes the final verdict: every check Some(true), failures listed by name.
    pub(crate) fn finalize(
        &mut self,
        integration: bool,
        dialog_enabled: bool,
    ) -> (String, Vec<String>) {
        let created = if integration { self.created() } else { None };
        let forwarding = if integration {
            Some(self.forwarding_ok(PROBE_EVENTS))
        } else {
            None
        };
        let dialog_render_continuity = if dialog_enabled {
            Some(
                self.presents_baseline > 0
                    && self.presents_during as f64 / self.presents_baseline as f64
                        >= DIALOG_CONTINUITY_MIN,
            )
        } else {
            None
        };
        let resize_no_timeouts = Some(self.resize_timeouts == 0);
        let shutdown_clean = if integration {
            Some(
                self.cleanup.tray == Some(true)
                    && self.cleanup.menu == Some(true)
                    && self.cleanup.hotkey == Some(true),
            )
        } else {
            Some(true)
        };
        let checks = [
            ("created", created),
            ("forwarding", forwarding),
            ("dialog_render_continuity", dialog_render_continuity),
            ("resize_no_timeouts", resize_no_timeouts),
            ("shutdown_clean", shutdown_clean),
        ];
        let failed: Vec<String> = checks
            .iter()
            .filter(|(_, value)| value == &Some(false))
            .map(|(name, _)| (*name).to_owned())
            .collect();
        let verdict = if failed.is_empty() { "ok" } else { "failed" };
        let json = format!(
            "{{\"verdict\":\"{verdict}\",\"created\":{},\"forwarding\":{},\"dialog_render_continuity\":{},\"resize_no_timeouts\":{},\"shutdown_clean\":{},\"resize_timeouts\":{},\"resize_requested\":{},\"tray_events\":{},\"menu_events\":{},\"hotkey_events\":{},\"probes_next\":{},\"failed\":{:?}}}",
            Self::flag(created),
            Self::flag(forwarding),
            Self::flag(dialog_render_continuity),
            Self::flag(resize_no_timeouts),
            Self::flag(shutdown_clean),
            self.resize_timeouts,
            self.resize_requested,
            self.tray_events,
            self.menu_events,
            self.hotkey_events,
            self.probe_next,
            failed,
        );
        (json, failed)
    }
}
