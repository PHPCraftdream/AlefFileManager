// SPDX-License-Identifier: MIT OR Apache-2.0
//! Tests of the pool FSM: pure, deterministic, no sleeps.

use core::time::Duration;

use super::backoff::{Clock, Jitter};
use super::config::PoolConfig;
use super::pool::{Pool, Pressure, Take};
use super::state::{Action, Level, SlotKind, SpareId};

#[derive(Clone, Copy)]
struct FakeClock(Duration);

impl Clock for FakeClock {
    fn now(&self) -> Duration {
        self.0
    }
}

struct ConstJitter(f64);

impl Jitter for ConstJitter {
    fn fraction(&mut self) -> f64 {
        self.0
    }
}

fn cfg() -> PoolConfig {
    PoolConfig {
        level: Level::Full,
        ..PoolConfig::default()
    }
}

fn pool() -> Pool<FakeClock, ConstJitter> {
    Pool::new(cfg(), FakeClock(Duration::ZERO), ConstJitter(0.0))
}

fn pool_no_breaker() -> Pool<FakeClock, ConstJitter> {
    Pool::new(
        PoolConfig {
            max_consecutive_failures: u32::MAX,
            ..cfg()
        },
        FakeClock(Duration::ZERO),
        ConstJitter(0.0),
    )
}

fn t(ms: u64) -> Duration {
    Duration::from_millis(ms)
}

fn id(v: u64) -> SpareId {
    SpareId(v)
}

/// Fill with two spares and mark both ready. Ids are deterministic: 1 and 2.
fn fill_and_ready(p: &mut Pool<FakeClock, ConstJitter>) -> (SpareId, SpareId) {
    let actions = p.fill(t(0));
    assert_eq!(actions.len(), 2);
    let w = match actions[0] {
        Action::Spawn { id, kind, level } => {
            assert_eq!(kind, SlotKind::Windowed);
            assert_eq!(level, Level::Full);
            id
        }
        _ => panic!("expected Spawn"),
    };
    let l = match actions[1] {
        Action::Spawn { id, kind, .. } => {
            assert_eq!(kind, SlotKind::Windowless);
            id
        }
        _ => panic!("expected Spawn"),
    };
    p.on_ready(w, t(1));
    p.on_ready(l, t(1));
    (w, l)
}

fn spawn_id(action: &Action) -> SpareId {
    match action {
        Action::Spawn { id, .. } => *id,
        _ => panic!("expected Spawn, got {action:?}"),
    }
}

trait TakeExt {
    /// Unwrap `Unavailable` with exactly one action (a refill Spawn).
    fn one_spawn(self) -> Action;
}

impl TakeExt for Take {
    fn one_spawn(self) -> Action {
        match self {
            Take::Unavailable { mut actions } => {
                assert_eq!(actions.len(), 1);
                actions.remove(0)
            }
            _ => panic!("expected Unavailable with one Spawn"),
        }
    }
}

// -- выдача ------------------------------------------------------------------

#[test]
fn grant_is_one_shot_and_schedules_replacement_in_same_call() {
    let mut p = pool();
    let (w, _) = fill_and_ready(&mut p);
    match p.take(SlotKind::Windowed, t(2)) {
        Take::Granted { id, actions } => {
            assert_eq!(id, w);
            assert_eq!(actions.len(), 1);
            let repl = spawn_id(&actions[0]);
            assert_ne!(repl, w);
            assert!(matches!(
                actions[0],
                Action::Spawn {
                    kind: SlotKind::Windowed,
                    ..
                }
            ));
        }
        _ => panic!("expected Granted"),
    }
    // The same process is never granted twice.
    assert!(matches!(
        p.take(SlotKind::Windowed, t(3)),
        Take::Unavailable { .. }
    ));
}

#[test]
fn replacement_spawn_comes_before_any_on_ready() {
    let mut p = pool();
    let _ = fill_and_ready(&mut p);
    let repl = match p.take(SlotKind::Windowless, t(2)) {
        Take::Granted { actions, .. } => {
            assert_eq!(actions.len(), 1);
            spawn_id(&actions[0])
        }
        _ => panic!("expected Granted"),
    };
    p.on_ready(repl, t(3));
    // The replacement is grantable without any further fill.
    match p.take(SlotKind::Windowless, t(4)) {
        Take::Granted { id, .. } => assert_eq!(id, repl),
        _ => panic!("expected Granted"),
    }
}

#[test]
fn empty_pool_take_is_unavailable_but_not_blocking() {
    let mut p = pool();
    let action = p.take(SlotKind::Windowed, t(0)).one_spawn();
    assert!(matches!(
        action,
        Action::Spawn {
            kind: SlotKind::Windowed,
            ..
        }
    ));
}

#[test]
fn volley_of_takes_creates_at_most_one_initializing_per_kind() {
    let mut p = pool();
    let mut spawn_ids = Vec::new();
    for i in 0..10u64 {
        if let Take::Unavailable { actions } = p.take(SlotKind::Windowed, t(i)) {
            spawn_ids.extend(actions.iter().map(spawn_id));
        }
    }
    assert_eq!(spawn_ids.len(), 1, "only the first volley take may spawn");
    p.on_ready(spawn_ids[0], t(10));
    assert!(matches!(
        p.take(SlotKind::Windowed, t(11)),
        Take::Granted { .. }
    ));
}

#[test]
fn take_while_initializing_or_backoff_returns_no_actions() {
    let mut p = pool();
    let a = p.take(SlotKind::Windowed, t(0)).one_spawn();
    // Initializing: no second spawn for the same kind.
    match p.take(SlotKind::Windowed, t(1)) {
        Take::Unavailable { actions } => assert!(actions.is_empty()),
        _ => panic!("expected Unavailable"),
    }
    p.on_failed(a, t(1));
    // Backoff: still no spawn, take falls back to cold start.
    match p.take(SlotKind::Windowed, t(2)) {
        Take::Unavailable { actions } => assert!(actions.is_empty()),
        _ => panic!("expected Unavailable"),
    }
}

#[test]
fn two_kinds_are_independent() {
    let mut p = pool();
    let (w, l) = fill_and_ready(&mut p);
    match p.take(SlotKind::Windowed, t(2)) {
        Take::Granted { id, actions } => {
            assert_eq!(id, w);
            assert!(actions.iter().all(|a| matches!(
                a,
                Action::Spawn {
                    kind: SlotKind::Windowed,
                    ..
                }
            )));
        }
        _ => panic!("expected Granted"),
    }
    // The windowless slot is untouched: still ready and grantable.
    match p.take(SlotKind::Windowless, t(3)) {
        Take::Granted { id, .. } => assert_eq!(id, l),
        _ => panic!("expected Granted for windowless"),
    }
    // Both replacements are already initializing: fill adds no second spawn per kind.
    assert!(p.fill(t(4)).is_empty());
}

// -- backoff и circuit breaker -------------------------------------------------

#[test]
fn failed_replacement_backs_off_and_grows_to_cap() {
    let mut p = pool_no_breaker();
    let mut current = p.take(SlotKind::Windowed, t(0)).one_spawn();
    let mut now = t(0);
    // Consecutive failures double the delay: 250, 500, 1000, 2000, 4000 ms.
    for expected_ms in [250u64, 500, 1000, 2000, 4000] {
        p.on_failed(current, now);
        now = now + t(expected_ms);
        assert!(p.fill(now - t(1)).is_empty(), "backoff not elapsed yet");
        let actions = p.fill(now);
        assert_eq!(actions.len(), 1);
        current = spawn_id(&actions[0]);
    }
    // The cap is 8s and stays there.
    p.on_failed(current, now);
    now = now + t(8_000);
    assert!(p.fill(now - t(1)).is_empty());
    let actions = p.fill(now);
    assert_eq!(actions.len(), 1);
    let again = spawn_id(&actions[0]);
    p.on_failed(again, now);
    now = now + t(8_000);
    assert!(p.fill(now - t(1)).is_empty());
    assert_eq!(p.fill(now).len(), 1);
}

#[test]
fn success_resets_backoff() {
    let mut p = pool_no_breaker();
    let a = p.take(SlotKind::Windowed, t(0)).one_spawn();
    p.on_failed(a, t(1)); // failures = 1 -> 250ms
    let actions = p.fill(t(251));
    let b = spawn_id(&actions[0]);
    p.on_ready(b, t(300)); // success resets the counter
    p.on_failed(b, t(301)); // first failure again -> base delay
    assert!(p.fill(t(400)).is_empty());
    assert_eq!(p.fill(t(551)).len(), 1);
}

#[test]
fn circuit_breaker_pauses_after_consecutive_failures() {
    let mut p = pool();
    let mut current = p.take(SlotKind::Windowed, t(0)).one_spawn();
    let mut now = t(0);
    // Three ordinary failures keep the exponential schedule.
    for expected_ms in [250u64, 500, 1000] {
        p.on_failed(current, now);
        now = now + t(expected_ms) + t(1);
        let actions = p.fill(now);
        assert_eq!(actions.len(), 1);
        current = spawn_id(&actions[0]);
    }
    // The fourth consecutive failure trips the breaker: a 30s pause.
    p.on_failed(current, now);
    let wake = p.next_wake().expect("paused slot reports a wake");
    assert_eq!(wake - now, t(30_000));
    assert!(p.fill(now + t(29_999)).is_empty());
    match p.take(SlotKind::Windowed, now + t(29_999)) {
        Take::Unavailable { actions } => assert!(actions.is_empty()),
        _ => panic!("expected Unavailable during the pause"),
    }
    let actions = p.fill(now + t(30_000));
    assert_eq!(actions.len(), 1);
}

#[test]
fn exit_of_ready_spare_refills_only_after_backoff() {
    let mut p = pool();
    let (w, _) = fill_and_ready(&mut p);
    p.on_exited(w, t(2));
    match p.take(SlotKind::Windowed, t(3)) {
        Take::Unavailable { actions } => assert!(actions.is_empty()),
        _ => panic!("expected Unavailable"),
    }
    assert!(p.fill(t(200)).is_empty());
    let actions = p.fill(t(252));
    assert_eq!(actions.len(), 1);
    assert!(matches!(
        actions[0],
        Action::Spawn {
            kind: SlotKind::Windowed,
            ..
        }
    ));
}

#[test]
fn unknown_ids_are_ignored() {
    let mut p = pool();
    let (w, _) = fill_and_ready(&mut p);
    p.on_ready(id(999), t(2));
    p.on_failed(id(999), t(2));
    p.on_exited(id(999), t(2));
    match p.take(SlotKind::Windowed, t(3)) {
        Take::Granted { id, .. } => assert_eq!(id, w),
        _ => panic!("expected Granted"),
    }
}

// -- давление памяти -------------------------------------------------------------

#[test]
fn mild_pressure_kills_ready_spares_first_and_pauses_refill() {
    let mut p = pool();
    let (w, l) = fill_and_ready(&mut p);
    let actions = p.release_pressure(t(2), Pressure::Mild);
    assert_eq!(
        actions,
        vec![Action::Kill { id: w }, Action::Kill { id: l }]
    );
    assert!(
        p.fill(t(3)).is_empty(),
        "no immediate refill under pressure"
    );
    let wake = p.next_wake().expect("wake after pressure backoff");
    assert_eq!(wake - t(2), t(2_000));
    assert_eq!(p.fill(t(2) + t(2_000)).len(), 2);
}

#[test]
fn severe_pressure_disables_windowed_until_restore() {
    let mut p = pool();
    let (w, _) = fill_and_ready(&mut p);
    let actions = p.release_pressure(t(2), Pressure::Severe);
    assert!(actions.contains(&Action::Kill { id: w }));
    // After the pressure backoff only the windowless slot refills.
    let actions = p.fill(t(2) + t(2_000));
    assert_eq!(actions.len(), 1);
    assert!(matches!(
        actions[0],
        Action::Spawn {
            kind: SlotKind::Windowless,
            ..
        }
    ));
    // Restore before the backoff elapses is a no-op.
    assert!(p.restore(t(2) + t(1_999)).is_empty());
    // After the backoff restore refills the GL slot.
    let actions = p.restore(t(2) + t(2_000));
    assert_eq!(actions.len(), 1);
    assert!(matches!(
        actions[0],
        Action::Spawn {
            kind: SlotKind::Windowed,
            ..
        }
    ));
}

#[test]
fn severe_pressure_kills_initializing_windowed_spare() {
    let mut p = Pool::new(
        PoolConfig {
            windowless: false,
            ..cfg()
        },
        FakeClock(Duration::ZERO),
        ConstJitter(0.0),
    );
    let _ = p.take(SlotKind::Windowed, t(0)); // initializing, nothing ready
    let actions = p.release_pressure(t(1), Pressure::Severe);
    assert_eq!(actions.len(), 1);
    assert!(matches!(actions[0], Action::Kill { .. }));
    assert!(p.fill(t(2)).is_empty());
}

// -- shutdown и выключенный пул ---------------------------------------------------

#[test]
fn shutdown_kills_owned_spares_and_stops_serving() {
    let mut p = pool();
    let (w, _) = fill_and_ready(&mut p);
    let granted = match p.take(SlotKind::Windowless, t(2)) {
        Take::Granted { id, .. } => id,
        _ => panic!("expected Granted"),
    };
    let actions = p.shutdown();
    assert!(actions.contains(&Action::Kill { id: w }));
    // The already granted process is not known to the pool and never killed.
    assert!(!actions.contains(&Action::Kill { id: granted }));
    assert!(p.fill(t(3)).is_empty());
    match p.take(SlotKind::Windowed, t(3)) {
        Take::Unavailable { actions } => assert!(actions.is_empty()),
        _ => panic!("expected Unavailable after shutdown"),
    }
    assert_eq!(p.next_wake(), None);
}

#[test]
fn disabled_pool_stays_empty_and_always_unavailable() {
    let mut p = Pool::new(
        PoolConfig {
            enabled: false,
            ..cfg()
        },
        FakeClock(Duration::ZERO),
        ConstJitter(0.0),
    );
    assert!(p.fill(t(0)).is_empty());
    for kind in SlotKind::ALL {
        match p.take(kind, t(1)) {
            Take::Unavailable { actions } => assert!(actions.is_empty()),
            _ => panic!("expected Unavailable"),
        }
    }
    assert_eq!(p.next_wake(), None);
    assert!(p.release_pressure(t(2), Pressure::Severe).is_empty());
    assert!(p.restore(t(3)).is_empty());
    assert!(p.shutdown().is_empty());
}

#[test]
fn selectively_disabled_kind_is_untouched() {
    let mut p = Pool::new(
        PoolConfig {
            windowed: false,
            ..cfg()
        },
        FakeClock(Duration::ZERO),
        ConstJitter(0.0),
    );
    let actions = p.fill(t(0));
    assert_eq!(actions.len(), 1);
    assert!(matches!(
        actions[0],
        Action::Spawn {
            kind: SlotKind::Windowless,
            ..
        }
    ));
    match p.take(SlotKind::Windowed, t(1)) {
        Take::Unavailable { actions } => assert!(actions.is_empty()),
        _ => panic!("expected Unavailable"),
    }
}

// -- детерминированность и вспомогательные -----------------------------------------

#[test]
fn deterministic_given_same_inputs() {
    let run = || {
        let mut p = Pool::new(cfg(), FakeClock(Duration::ZERO), ConstJitter(0.25));
        let mut log = Vec::new();
        log.extend(p.fill(t(0)));
        p.on_ready(id(1), t(1));
        p.on_ready(id(2), t(1));
        if let Take::Granted { id, actions } = p.take(SlotKind::Windowed, t(2)) {
            log.push(Action::Kill { id });
            log.extend(actions);
        }
        p.on_exited(id(3), t(3)); // the replacement died
        log.extend(p.fill(t(4_000)));
        log
    };
    let a = run();
    let b = run();
    assert_eq!(a, b);
    assert!(a.len() >= 3);
}

#[test]
fn jitter_shifts_backoff_deterministically() {
    let delay = |frac: f64| {
        let mut p = Pool::new(cfg(), FakeClock(Duration::ZERO), ConstJitter(frac));
        let a = p.take(SlotKind::Windowed, t(0)).one_spawn();
        p.on_failed(a, t(1));
        p.next_wake().unwrap() - t(1)
    };
    assert_eq!(delay(0.0), t(250));
    assert_eq!(delay(0.5), t(312)); // 250 * (1 + 0.25) = 312.5 ms
    assert!(delay(0.0) < delay(0.9));
}

#[test]
fn next_wake_reports_earliest_backoff() {
    let mut p = pool();
    assert_eq!(p.next_wake(), None);
    let a = p.take(SlotKind::Windowed, t(0)).one_spawn();
    let b = p.take(SlotKind::Windowless, t(0)).one_spawn();
    p.on_failed(a, t(1));
    let wake_w = p.next_wake().unwrap();
    p.on_failed(b, t(500));
    let wake_both = p.next_wake().unwrap();
    assert_eq!(wake_both, wake_w.min(t(500) + t(250)));
}
