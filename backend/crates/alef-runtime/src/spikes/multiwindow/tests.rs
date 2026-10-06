// SPDX-License-Identifier: MIT OR Apache-2.0
use std::time::{Duration, Instant};

use super::verdict::{
    confirm_budget, load_timed_out, route, step_size, step_target, timeout_tolerance, verdict,
    Target, VerdictInput, ALT_STEPS, VERIFY_STEPS,
};
use super::{Phase, Spike};

type Mutator = fn(&mut VerdictInput);

fn passing_input() -> VerdictInput {
    VerdictInput {
        w1_loaded: true,
        w2_loaded: true,
        requested: 35,
        confirmed: 35,
        timeouts: 1,
        timeout_tolerance: 1,
        crashes: 0,
        w2_closed_w1_alive: true,
        clean_exit: true,
    }
}

#[test]
fn route_covers_all_branches_with_secondary_precedence() {
    assert_eq!(route(2, Some(1), Some(2)), Target::Secondary);
    assert_eq!(route(1, Some(1), Some(2)), Target::Primary);
    assert_eq!(route(3, Some(1), Some(2)), Target::Unknown);
    assert_eq!(route(1, None, None), Target::Unknown);
    assert_eq!(route(1, Some(1), Some(1)), Target::Secondary);
}

#[test]
fn verdict_passes_only_when_every_check_holds() {
    let result = verdict(passing_input());
    assert!(result.passed);
    assert!(result.resizes_confirmed && result.timeouts_within_tolerance && result.no_crash);
}

#[test]
fn verdict_fails_on_each_single_bad_input() {
    let cases: Vec<(&str, Mutator)> = vec![
        ("w1_loaded", |i| i.w1_loaded = false),
        ("w2_loaded", |i| i.w2_loaded = false),
        ("confirmed", |i| i.confirmed -= 1),
        ("tolerance", |i| i.timeouts += 1),
        ("crash", |i| i.crashes += 1),
        ("w2_closed_w1_alive", |i| i.w2_closed_w1_alive = false),
        ("clean_exit", |i| i.clean_exit = false),
    ];
    for (name, mutate) in cases {
        let mut input = passing_input();
        mutate(&mut input);
        let result = verdict(input);
        assert!(!result.passed, "{name} must fail the verdict");
    }
}

#[test]
fn verdict_rejects_zero_requested_steps() {
    let mut input = passing_input();
    input.requested = 0;
    input.confirmed = 0;
    assert!(
        !verdict(input).resizes_confirmed,
        "no vacuous pass without steps"
    );
}

#[test]
fn next_wake_returns_exact_instants_per_phase() {
    let mut spike = Spike::new();
    let step_at = Instant::now() + Duration::from_secs(10);
    let settle = Duration::from_millis(800);
    spike.deadline = step_at;
    assert_eq!(spike.next_wake_via_test(), Some(step_at));
    spike.phase = Phase::AlternateResize;
    spike.next_step = step_at;
    spike.step = 3;
    assert_eq!(spike.next_wake_via_test(), Some(step_at));
    spike.step = ALT_STEPS;
    assert_eq!(spike.next_wake_via_test(), Some(step_at + settle));
    spike.phase = Phase::VerifyPrimary;
    spike.settled = true;
    spike.step = 2;
    assert_eq!(spike.next_wake_via_test(), Some(step_at));
    spike.step = VERIFY_STEPS;
    assert_eq!(spike.next_wake_via_test(), Some(step_at + settle));
    spike.phase = Phase::ClosePrimary;
    assert_eq!(spike.next_wake_via_test(), Some(step_at));
    spike.phase = Phase::Done;
    assert_eq!(spike.next_wake_via_test(), None);
}

#[test]
fn strict_mode_disables_tolerance_and_budget() {
    assert_eq!(timeout_tolerance(false), 1);
    assert_eq!(timeout_tolerance(true), 0);
    assert_eq!(confirm_budget(false), Duration::from_secs(2));
    assert_eq!(confirm_budget(true), Duration::from_millis(1));
}

#[test]
fn load_deadline_is_enforced() {
    let now = Instant::now();
    assert!(load_timed_out(now, now - Duration::from_secs(1)));
    assert!(!load_timed_out(now, now + Duration::from_secs(1)));
}

#[test]
fn step_plan_alternates_windows_and_ends_at_default() {
    assert_eq!(step_target(0, false), Target::Primary);
    assert_eq!(step_target(1, false), Target::Secondary);
    assert_eq!(step_target(ALT_STEPS - 1, false), Target::Secondary);
    for step in 0..VERIFY_STEPS {
        assert_eq!(step_target(step, true), Target::Primary);
        let (width, height) = step_size(step, true);
        assert!((800.0..=1400.0).contains(&width) && (600.0..=900.0).contains(&height));
    }
    assert_eq!(step_size(VERIFY_STEPS - 1, true), (1200.0, 800.0));
}
