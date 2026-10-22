// SPDX-License-Identifier: MIT OR Apache-2.0
//! Broker-side request log: bounded, keyed by request-id, injected time only.
//!
//! Purpose: after a connection drops, the broker can still answer
//! `Query(request_id)` with the recorded outcome, and a repeated `Launch`
//! with the same request-id is IDEMPOTENT — it returns the earlier decision
//! and never triggers a second launch.
//!
//! Time is always a parameter (`now_ms`, milliseconds); the log never reads
//! a clock. `query` cannot distinguish "never seen" from "seen but expired":
//! both return `LaunchState::Unknown`, and the caller translates that into
//! `Unknown` vs `UnknownOutcome` per its own restart knowledge (a broker
//! restart means the log may be incomplete ⇒ `UnknownOutcome`).

use std::collections::VecDeque;

use super::message::{DeclineReason, LaunchState, RequestId};

/// Hard cap on the number of entries, independent of the configured capacity.
pub const MAX_LOG_ENTRIES: usize = 1024;

/// The broker's answer to a `Launch`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Decision {
    Accepted,
    Declined(DeclineReason),
}

/// What happened after acceptance.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    HandedOff,
    Failed(String),
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct Entry {
    id: RequestId,
    decision: Decision,
    outcome: Option<Outcome>,
    /// `decision` is `Declined` (final from creation) or `outcome.is_some()`.
    finished: bool,
    at_ms: u64,
}

impl Entry {
    fn state(&self) -> LaunchState {
        match (&self.decision, &self.outcome) {
            (_, Some(Outcome::HandedOff)) => LaunchState::HandedOff,
            (_, Some(Outcome::Failed(reason))) => LaunchState::Failed(reason.clone()),
            (Decision::Accepted, None) => LaunchState::Accepted,
            (Decision::Declined(reason), None) => LaunchState::Failed(reason_text(reason)),
        }
    }
}

/// A declined request is finished, so it maps to a terminal state; there is
/// no distinct declined value in `LaunchState`, the reason travels as
/// `Failed`. (The broker reports `Declined` on the wire itself; the log only
/// answers later `Query` calls.)
fn reason_text(reason: &DeclineReason) -> String {
    format!("declined: {reason:?}")
}

/// Bounded FIFO of launch decisions, ordered oldest-first.
///
/// A bounded log must never grow unbounded: when at capacity and no entry is
/// finished, the OLDEST entry is evicted anyway (fail-closed for memory, at
/// the cost of an answer becoming `Unknown`; the client treats that as
/// ambiguity, never as proof of non-acceptance).
#[derive(Debug)]
pub struct RequestLog {
    capacity: usize,
    retain_ms: u64,
    entries: VecDeque<Entry>,
}

impl RequestLog {
    /// `capacity` is clamped to `[1, MAX_LOG_ENTRIES]`.
    pub fn new(capacity: usize, retain_ms: u64) -> RequestLog {
        RequestLog {
            capacity: capacity.clamp(1, MAX_LOG_ENTRIES),
            retain_ms,
            entries: VecDeque::new(),
        }
    }

    /// Idempotency point. If `id` already exists in ANY state, return the
    /// RECORDED decision unchanged — never overwrite, never create a second
    /// entry. Otherwise record and return the new decision.
    pub fn decide(&mut self, id: &RequestId, decision: Decision, now_ms: u64) -> Decision {
        if let Some(entry) = self.entries.iter().find(|e| &e.id == id) {
            return entry.decision.clone();
        }
        self.evict(now_ms);
        let finished = matches!(decision, Decision::Declined(_));
        self.entries.push_back(Entry {
            id: id.clone(),
            decision: decision.clone(),
            outcome: None,
            finished,
            at_ms: now_ms,
        });
        decision
    }

    /// Mark an accepted entry as `HandedOff` (finished). `false` if the id is
    /// unknown or the entry is already finished (`Failed` is final too;
    /// `Declined` entries are finished from creation).
    pub fn handoff(&mut self, id: &RequestId, now_ms: u64) -> bool {
        self.finish(id, Outcome::HandedOff, now_ms)
    }

    /// Mark an accepted entry as `Failed` (final). `false` if the id is
    /// unknown or not open.
    pub fn fail(&mut self, id: &RequestId, reason: String, now_ms: u64) -> bool {
        self.finish(id, Outcome::Failed(reason), now_ms)
    }

    fn finish(&mut self, id: &RequestId, outcome: Outcome, now_ms: u64) -> bool {
        let Some(entry) = self.entries.iter_mut().find(|e| &e.id == id) else {
            return false;
        };
        if entry.finished {
            return false;
        }
        entry.outcome = Some(outcome);
        entry.finished = true;
        // Finished entries age from their finishing time.
        entry.at_ms = now_ms;
        true
    }

    /// Pure read with lazy expiry. `Unknown` for a never-seen id AND for an
    /// entry older than `retain_ms` (expired entries are evicted on read).
    /// The caller — not the log — distinguishes the two cases.
    pub fn query(&mut self, id: &RequestId, now_ms: u64) -> LaunchState {
        let expired = self
            .entries
            .iter()
            .find(|e| &e.id == id)
            .is_some_and(|e| now_ms.saturating_sub(e.at_ms) > self.retain_ms);
        if expired {
            self.entries.retain(|e| &e.id != id);
            return LaunchState::Unknown;
        }
        self.entries
            .iter()
            .find(|e| &e.id == id)
            .map(Entry::state)
            .unwrap_or(LaunchState::Unknown)
    }

    pub fn len(&self) -> usize {
        self.entries.len()
    }

    pub fn is_empty(&self) -> bool {
        self.entries.is_empty()
    }

    /// Expire entries past retention (finished ones only age out this way;
    /// an unfinished entry is only ever dropped by capacity pressure), then
    /// enforce capacity: oldest finished first; if nothing is finished, the
    /// oldest entry regardless of state — a bounded log must never grow
    /// unbounded.
    fn evict(&mut self, now_ms: u64) {
        self.entries
            .retain(|e| now_ms.saturating_sub(e.at_ms) <= self.retain_ms);
        while self.entries.len() >= self.capacity {
            let finished = self.entries.iter().position(|e| e.finished);
            let idx = finished.unwrap_or(0);
            self.entries.remove(idx);
        }
    }

    #[cfg(test)]
    pub(crate) fn capacity(&self) -> usize {
        self.capacity
    }

    #[cfg(test)]
    pub(crate) fn entries_snapshot(&self) -> Vec<(RequestId, Decision, Option<Outcome>)> {
        self.entries
            .iter()
            .map(|e| (e.id.clone(), e.decision.clone(), e.outcome.clone()))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::message::UnsupportedReason;

    fn declined(reason: DeclineReason) -> Decision {
        Decision::Declined(reason)
    }

    #[test]
    fn decide_is_idempotent_accepted_first() {
        let mut log = RequestLog::new(4, 1_000);
        assert_eq!(
            log.decide(&"r1".into(), Decision::Accepted, 10),
            Decision::Accepted
        );
        // Same id, conflicting decision: the recorded one wins.
        assert_eq!(
            log.decide(&"r1".into(), declined(DeclineReason::NoSpare), 20),
            Decision::Accepted
        );
        assert_eq!(log.len(), 1);
    }

    #[test]
    fn decide_is_idempotent_declined_first() {
        let mut log = RequestLog::new(4, 1_000);
        assert_eq!(
            log.decide(&"r1".into(), declined(DeclineReason::Busy), 10),
            declined(DeclineReason::Busy)
        );
        assert_eq!(
            log.decide(&"r1".into(), Decision::Accepted, 20),
            declined(DeclineReason::Busy)
        );
        assert_eq!(log.len(), 1);
    }

    #[test]
    fn handoff_and_fail_transitions() {
        let mut log = RequestLog::new(4, 1_000);
        log.decide(&"a".into(), Decision::Accepted, 10);
        log.decide(&"b".into(), Decision::Accepted, 10);
        log.decide(&"c".into(), declined(DeclineReason::NoSpare), 10);
        assert!(log.handoff(&"a".into(), 20));
        assert!(!log.handoff(&"a".into(), 30)); // already finished
        assert!(log.fail(&"b".into(), "boom".into(), 20));
        assert!(!log.fail(&"b".into(), "again".into(), 30)); // Failed is final
        assert!(!log.handoff(&"c".into(), 20)); // Declined: finished from creation
        assert!(!log.handoff(&"zz".into(), 20)); // unknown
    }

    #[test]
    fn idempotent_decide_after_handoff_keeps_original() {
        let mut log = RequestLog::new(4, 1_000);
        log.decide(&"a".into(), Decision::Accepted, 10);
        assert!(log.handoff(&"a".into(), 20));
        assert_eq!(
            log.decide(&"a".into(), Decision::Accepted, 30),
            Decision::Accepted
        );
        assert_eq!(log.query(&"a".into(), 30), LaunchState::HandedOff); // not reset
    }

    #[test]
    fn query_unknown_for_never_seen() {
        let mut log = RequestLog::new(4, 1_000);
        assert_eq!(log.query(&"nope".into(), 0), LaunchState::Unknown);
    }

    #[test]
    fn retain_expiry_hides_old_entry_only() {
        let mut log = RequestLog::new(4, 100);
        log.decide(&"old".into(), Decision::Accepted, 0);
        log.decide(&"fresh".into(), Decision::Accepted, 90);
        assert_eq!(log.query(&"old".into(), 101), LaunchState::Unknown); // 101 > 0+100
        assert_eq!(log.query(&"fresh".into(), 101), LaunchState::Accepted);
        assert_eq!(log.len(), 1); // expired entry evicted on read
    }

    #[test]
    fn query_maps_outcomes() {
        let mut log = RequestLog::new(4, 1_000);
        log.decide(&"h".into(), Decision::Accepted, 0);
        log.decide(&"f".into(), Decision::Accepted, 0);
        log.decide(&"d".into(), declined(DeclineReason::Busy), 0);
        assert!(log.handoff(&"h".into(), 1));
        assert!(log.fail(&"f".into(), "bind failed".into(), 1));
        assert_eq!(log.query(&"h".into(), 2), LaunchState::HandedOff);
        assert_eq!(
            log.query(&"f".into(), 2),
            LaunchState::Failed("bind failed".into())
        );
        assert_eq!(
            log.query(&"d".into(), 2),
            LaunchState::Failed("declined: Busy".into())
        );
    }

    #[test]
    fn capacity_evicts_oldest_finished_first() {
        let mut log = RequestLog::new(2, 10_000);
        log.decide(&"A".into(), Decision::Accepted, 0);
        log.decide(&"B".into(), Decision::Accepted, 1);
        assert!(log.handoff(&"A".into(), 2));
        log.decide(&"C".into(), Decision::Accepted, 3); // A is finished: evicted
        let snap = log.entries_snapshot();
        assert_eq!(snap.len(), 2);
        assert_eq!(snap[0].0, "B");
        assert_eq!(snap[1].0, "C");
        assert_eq!(log.query(&"A".into(), 3), LaunchState::Unknown);
    }

    #[test]
    fn capacity_evicts_oldest_even_when_nothing_finished() {
        let mut log = RequestLog::new(2, 10_000);
        log.decide(&"A".into(), Decision::Accepted, 0);
        log.decide(&"B".into(), Decision::Accepted, 1);
        log.decide(&"C".into(), Decision::Accepted, 2); // nothing finished: A evicted anyway
        let snap = log.entries_snapshot();
        assert_eq!(snap.len(), 2);
        assert_eq!(snap[0].0, "B");
        assert_eq!(snap[1].0, "C");
    }

    #[test]
    fn capacity_clamped_to_max() {
        assert_eq!(
            RequestLog::new(usize::MAX, 1_000).capacity(),
            MAX_LOG_ENTRIES
        );
        assert_eq!(RequestLog::new(0, 1_000).capacity(), 1);
    }

    #[test]
    fn capacity_is_absolute_hard_cap() {
        let mut log = RequestLog::new(MAX_LOG_ENTRIES, 10_000);
        for i in 0..(MAX_LOG_ENTRIES + 5) {
            log.decide(&format!("r{i}"), Decision::Accepted, i as u64);
        }
        assert_eq!(log.len(), MAX_LOG_ENTRIES);
    }

    #[test]
    fn declined_reason_variants_are_distinct_decisions() {
        let mut log = RequestLog::new(4, 1_000);
        assert_eq!(
            log.decide(
                &"u".into(),
                declined(DeclineReason::Unsupported(UnsupportedReason::Devtools)),
                0
            ),
            declined(DeclineReason::Unsupported(UnsupportedReason::Devtools))
        );
        assert_eq!(
            log.query(&"u".into(), 1),
            LaunchState::Failed("declined: Unsupported(Devtools)".into())
        );
    }
}
