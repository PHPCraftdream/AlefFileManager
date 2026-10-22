// SPDX-License-Identifier: MIT OR Apache-2.0
//! The pool finite-state machine: pure logic, no threads, no timers, no
//! processes. Every external effect is returned as an `Action`.

use core::time::Duration;

use super::backoff::{capped_delay, Clock, Jitter};
use super::config::PoolConfig;
use super::state::{Action, Level, Slot, SlotKind, SlotState, SpareId};

/// Severity of a memory-pressure event reported by the wiring.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Pressure {
    /// Kill ready spares to free memory.
    Mild,
    /// Additionally disable the Windowed (GL) slot until `restore`.
    Severe,
}

/// Result of a `take`.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Take {
    /// A ready spare was atomically granted and a replacement of the same
    /// kind was scheduled in the same operation. The granted process is
    /// one-shot: the pool never sees it again.
    Granted { id: SpareId, actions: Vec<Action> },
    /// No ready spare: the caller falls back to a cold start. The pool may
    /// still ask for a refill (when its slot is empty).
    Unavailable { actions: Vec<Action> },
}

/// Spare-process pool. Two logical slots at most, one per enabled kind;
/// at most one `Initializing` process per kind at any time.
pub struct Pool<C: Clock, J: Jitter> {
    cfg: PoolConfig,
    clock: C,
    jitter: J,
    next_id: u64,
    closed: bool,
    slots: [Slot; 2],
}

impl<C: Clock, J: Jitter> Pool<C, J> {
    pub fn new(cfg: PoolConfig, clock: C, jitter: J) -> Self {
        Pool {
            cfg,
            clock,
            jitter,
            next_id: 0,
            closed: false,
            slots: [
                Slot::new(SlotKind::Windowed),
                Slot::new(SlotKind::Windowless),
            ],
        }
    }

    /// Access to the injected clock (the pool itself only stores it; every
    /// event carries its own `now`).
    pub fn clock(&self) -> &C {
        &self.clock
    }

    /// Fill the pool up to its target state: spawn for empty slots and for
    /// slots whose backoff has elapsed. Never more than one spawn per kind.
    pub fn fill(&mut self, now: Duration) -> Vec<Action> {
        let mut actions = Vec::new();
        for kind in SlotKind::ALL {
            if !self.enabled(kind) || self.slots[kind.index()].disabled {
                continue;
            }
            if !self.refill_due(kind, now) {
                continue;
            }
            let (_, action) = self.start_spawn(kind, now);
            actions.push(action);
        }
        actions
    }

    /// A spare finished warming up. Unknown ids and stale ids are ignored.
    pub fn on_ready(&mut self, id: SpareId, now: Duration) -> Vec<Action> {
        let i = match self.slot_index_by_id(id) {
            Some(i) => i,
            None => return Vec::new(),
        };
        if matches!(self.slots[i].state, SlotState::Initializing { .. }) {
            self.slots[i].failures = 0;
            self.slots[i].state = SlotState::Ready { id, since: now };
        }
        Vec::new()
    }

    /// A spare failed. Exponential backoff with deterministic jitter; after
    /// `max_consecutive_failures` in a row the circuit breaker pauses the
    /// slot for `failure_pause`. A later success resets the counter.
    pub fn on_failed(&mut self, id: SpareId, now: Duration) -> Vec<Action> {
        self.retire(id, now);
        Vec::new()
    }

    /// A spare exited on its own (a ready spare died, or an initializing one
    /// crashed). The slot refills only after backoff.
    pub fn on_exited(&mut self, id: SpareId, now: Duration) -> Vec<Action> {
        self.retire(id, now);
        Vec::new()
    }

    /// Hand out a ready spare of `kind` and schedule its replacement in the
    /// same operation. Empty slot: refill request without blocking. Busy,
    /// backing off, disabled or closed pool: plain `Unavailable`.
    pub fn take(&mut self, kind: SlotKind, now: Duration) -> Take {
        if self.closed || !self.enabled(kind) || self.slots[kind.index()].disabled {
            return Take::Unavailable {
                actions: Vec::new(),
            };
        }
        match self.slots[kind.index()].state {
            SlotState::Ready { id, .. } => {
                let (_, spawn) = self.start_spawn(kind, now);
                Take::Granted {
                    id,
                    actions: vec![spawn],
                }
            }
            SlotState::Empty => {
                let (_, spawn) = self.start_spawn(kind, now);
                Take::Unavailable {
                    actions: vec![spawn],
                }
            }
            SlotState::Initializing { .. } | SlotState::Backoff { .. } => Take::Unavailable {
                actions: Vec::new(),
            },
        }
    }

    /// Memory pressure: first kill ready (unused) spares of both kinds; a
    /// severe pressure additionally disables the Windowed (GL) slot until
    /// `restore`. Killed slots refill only after `pressure_backoff`.
    pub fn release_pressure(&mut self, now: Duration, severity: Pressure) -> Vec<Action> {
        let mut actions = Vec::new();
        for kind in SlotKind::ALL {
            if !self.enabled(kind) {
                continue;
            }
            let i = kind.index();
            let ready = matches!(self.slots[i].state, SlotState::Ready { .. });
            let severe_gl = severity == Pressure::Severe && kind == SlotKind::Windowed;
            if !ready && !severe_gl {
                continue;
            }
            if let Some(id) = self.slots[i].id() {
                actions.push(Action::Kill { id });
            }
            self.slots[i].failures = 0;
            self.slots[i].disabled = severe_gl;
            self.slots[i].state = SlotState::Backoff {
                until: now + self.cfg.pressure_backoff,
                failures: 0,
            };
        }
        actions
    }

    /// Return the Windowed (GL) slot, but only after its backoff has elapsed;
    /// earlier calls are no-ops. Returns the resulting refill actions.
    pub fn restore(&mut self, now: Duration) -> Vec<Action> {
        let i = SlotKind::Windowed.index();
        if self.enabled(SlotKind::Windowed) && self.slots[i].disabled {
            if let SlotState::Backoff { until, .. } = self.slots[i].state {
                if now >= until {
                    self.slots[i].disabled = false;
                    self.slots[i].state = SlotState::Empty;
                }
            }
        }
        self.fill(now)
    }

    /// Kill every spare the pool still owns and stop serving `take`/`fill`.
    /// Already granted processes are unknown to the pool and untouched.
    pub fn shutdown(&mut self) -> Vec<Action> {
        let mut actions = Vec::new();
        self.closed = true;
        for slot in &mut self.slots {
            if let Some(id) = slot.id() {
                actions.push(Action::Kill { id });
            }
            slot.state = SlotState::Empty;
            slot.failures = 0;
            slot.disabled = false;
        }
        actions
    }

    /// When the wiring should call `fill` next; `None` means event-driven only.
    pub fn next_wake(&self) -> Option<Duration> {
        let mut wake = None;
        for kind in SlotKind::ALL {
            if !self.enabled(kind) {
                continue;
            }
            if let SlotState::Backoff { until, .. } = self.slots[kind.index()].state {
                wake = Some(match wake {
                    Some(earlier) => earlier.min(until),
                    None => until,
                });
            }
        }
        wake
    }

    // -- internals -------------------------------------------------------

    fn enabled(&self, kind: SlotKind) -> bool {
        self.cfg.enabled
            && match kind {
                SlotKind::Windowed => self.cfg.windowed,
                SlotKind::Windowless => self.cfg.windowless,
            }
    }

    fn refill_due(&self, kind: SlotKind, now: Duration) -> bool {
        match self.slots[kind.index()].state {
            SlotState::Empty => true,
            SlotState::Backoff { until, .. } => now >= until,
            SlotState::Initializing { .. } | SlotState::Ready { .. } => false,
        }
    }

    fn start_spawn(&mut self, kind: SlotKind, now: Duration) -> (SpareId, Action) {
        self.next_id += 1;
        let id = SpareId(self.next_id);
        self.slots[kind.index()].state = SlotState::Initializing { id, since: now };
        let action = Action::Spawn {
            id,
            kind,
            level: self.cfg.level,
        };
        (id, action)
    }

    fn slot_index_by_id(&self, id: SpareId) -> Option<usize> {
        SlotKind::ALL
            .iter()
            .position(|&k| self.slots[k.index()].id() == Some(id))
    }

    fn retire(&mut self, id: SpareId, now: Duration) {
        let i = match self.slot_index_by_id(id) {
            Some(i) => i,
            None => return,
        };
        self.slots[i].failures += 1;
        let failures = self.slots[i].failures;
        let until = if failures >= self.cfg.max_consecutive_failures {
            now + self.cfg.failure_pause
        } else {
            now + self.delay(failures - 1)
        };
        self.slots[i].state = SlotState::Backoff { until, failures };
    }

    fn delay(&mut self, failures: u32) -> Duration {
        let base = capped_delay(self.cfg.backoff_base, self.cfg.backoff_max, failures);
        let frac = self.jitter.fraction().clamp(0.0, 1.0);
        let ms = base.as_millis() as f64;
        Duration::from_millis((ms * (1.0 + 0.5 * frac)) as u64)
    }
}
