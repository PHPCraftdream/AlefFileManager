// SPDX-License-Identifier: MIT OR Apache-2.0
//! Tests of the acceptance point: a spare that declines, dies or times out after the
//! assignment, and the requests that arrive while the launch is pending.

use super::*;

/// Grant with a spare decline AFTER the Assign: the client (and the log) see
/// a proven decline, the slot goes to backoff, no launch is live.
#[test]
fn spare_declines_after_assign_client_gets_declined() {
    let (spawner, h) = harness("spare-declines-after-assign", pool_cfg());
    h.broker.pump();
    let ids = wait_ids(&h.rx, 2);
    let win = ids[0];
    let mut spare = connect_spare(&h.ep, WireKind::Windowed, win, spawner.proc_of(win).pid);
    spare_ready(&h, &mut spare);

    let mut client = connect(&h.ep);
    client.send(&ClientMsg::Launch(launch("r1"))).unwrap();
    let _ = recv_spare(&mut spare); // Assign → pending
    spare
        .send_raw(
            &encode_spare_to_broker(&SpareToBroker::Declined(SpareDeclined {
                reason: "no window".into(),
            }))
            .unwrap(),
        )
        .unwrap();
    assert!(matches!(
        recv_broker(&mut client),
        BrokerMsg::Declined(Declined {
            reason: DeclineReason::ValidationFailed,
            ..
        })
    ));

    // The spare was killed; only the untouched windowless one remains, and
    // the windowed slot is in backoff (set synchronously on the decline).
    assert!(spawner.proc_of(win).killed.load(Ordering::SeqCst));
    let mut admin = connect(&h.ep);
    match admin_call(&mut admin, WarmAdminMsg::Status(AdminStatus)) {
        WarmAdminReply::StatusReply(s) => {
            // The untouched windowless spare and the replacement of the windowed slot.
            assert_eq!(s.running, 2);
            let windowed = s
                .slots
                .iter()
                .find(|e| e.kind == WireKind::Windowed)
                .expect("windowed slot");
            // The slot belongs to the replacement that the grant started.
            assert_eq!(windowed.state, "initializing");
        }
        other => panic!("expected StatusReply, got {other:?}"),
    }

    // A query from a NEW connection resolves the recorded decline.
    let mut probe = connect(&h.ep);
    probe
        .send(&ClientMsg::Query(Query {
            request_id: "r1".into(),
        }))
        .unwrap();
    assert!(matches!(
        recv_broker(&mut probe),
        BrokerMsg::Status(Status {
            state: LaunchState::Failed(ref m),
            ..
        }) if m == "declined: ValidationFailed"
    ));
    h.stop();
}

/// The spare dies between the Assign and `Started`: the pending launch is
/// declined with NoSpare on the next pump.
#[test]
fn spare_dies_after_assign_declines_no_spare() {
    let (spawner, h) = harness("spare-dies-after-assign", pool_cfg());
    h.broker.pump();
    let ids = wait_ids(&h.rx, 2);
    let win = ids[0];
    let mut spare = connect_spare(&h.ep, WireKind::Windowed, win, spawner.proc_of(win).pid);
    spare_ready(&h, &mut spare);

    let mut client = connect(&h.ep);
    client.send(&ClientMsg::Launch(launch("r1"))).unwrap();
    let _ = recv_spare(&mut spare); // Assign → pending

    spawner.proc_of(win).exit_with(1);
    h.broker.pump(); // notices the pending spare's death → Declined{NoSpare}
    assert!(matches!(
        recv_broker_deadline(&mut client),
        BrokerMsg::Declined(Declined {
            reason: DeclineReason::NoSpare,
            ..
        })
    ));
    h.stop();
}

/// The assign deadline passes before `Started`: the pending launch is
/// declined and the spare process is killed.
#[test]
fn assign_timeout_declines_and_kills() {
    let (spawner, h) = harness("assign-timeout", pool_cfg());
    h.broker.set_assign_timeout(Duration::from_millis(50));
    h.broker.pump();
    let ids = wait_ids(&h.rx, 2);
    let win = ids[0];
    let mut spare = connect_spare(&h.ep, WireKind::Windowed, win, spawner.proc_of(win).pid);
    spare_ready(&h, &mut spare);

    let mut client = connect(&h.ep);
    client.send(&ClientMsg::Launch(launch("r1"))).unwrap();
    let _ = recv_spare(&mut spare); // Assign → pending

    h.clock.advance(Duration::from_millis(100));
    h.broker.pump();
    assert!(matches!(
        recv_broker_deadline(&mut client),
        BrokerMsg::Declined(Declined {
            reason: DeclineReason::ValidationFailed,
            ..
        })
    ));
    assert!(spawner.proc_of(win).killed.load(Ordering::SeqCst));
    h.stop();
}

/// A duplicate Launch (same rid) while a launch is pending attaches another
/// writer instead of taking a second spare; both connections receive the
/// outcome of the SAME launch.
#[test]
fn duplicate_launch_during_pending_shares_outcome() {
    let (spawner, h) = harness("duplicate-pending", pool_cfg());
    h.broker.pump();
    let ids = wait_ids(&h.rx, 2);
    let win = ids[0];
    let mut spare = connect_spare(&h.ep, WireKind::Windowed, win, spawner.proc_of(win).pid);
    spare_ready(&h, &mut spare);

    let mut a = connect(&h.ep);
    a.send(&ClientMsg::Launch(launch("r1"))).unwrap();
    let _ = recv_spare(&mut spare); // Assign → pending

    // Duplicate from a second connection: no answer now, no new spawn.
    let mut b = connect(&h.ep);
    b.send(&ClientMsg::Launch(launch("r1"))).unwrap();
    let replacement = h.rx_recv_one(); // the take's same-kind replacement
    assert_ne!(replacement, win);
    assert!(h.rx_try_recv().is_none(), "duplicate must not spawn");

    spare_started(&mut spare);
    assert!(matches!(recv_broker(&mut b), BrokerMsg::Accepted(_)));
    assert!(matches!(recv_broker(&mut a), BrokerMsg::Accepted(_)));

    // The relay keeps running on the original connection.
    let proc = spawner.proc_of(win);
    proc.out.push(b"dup");
    proc.out.close();
    proc.exit_with(3);
    let mut got = Vec::new();
    let deadline = Instant::now() + DEADLINE;
    loop {
        assert!(Instant::now() < deadline, "relay timeout");
        match recv_relay(&mut a) {
            WarmRelayMsg::Output(o) => got.extend(o.bytes),
            WarmRelayMsg::Exited(_) => break,
        }
    }
    assert_eq!(got, b"dup");
    h.stop();
}

/// Query while a launch is pending reports Accepted (the launch may already
/// be accepted at any moment), unknown ids stay Unknown.
#[test]
fn query_during_pending_reports_accepted() {
    let (spawner, h) = harness("query-pending", pool_cfg());
    h.broker.pump();
    let ids = wait_ids(&h.rx, 2);
    let win = ids[0];
    let mut spare = connect_spare(&h.ep, WireKind::Windowed, win, spawner.proc_of(win).pid);
    spare_ready(&h, &mut spare);

    let mut client = connect(&h.ep);
    client.send(&ClientMsg::Launch(launch("r1"))).unwrap();
    let _ = recv_spare(&mut spare); // Assign → pending

    let mut probe = connect(&h.ep);
    probe
        .send(&ClientMsg::Query(Query {
            request_id: "r1".into(),
        }))
        .unwrap();
    assert!(matches!(
        recv_broker(&mut probe),
        BrokerMsg::Status(Status {
            state: LaunchState::Accepted,
            ..
        })
    ));
    probe
        .send(&ClientMsg::Query(Query {
            request_id: "never-seen".into(),
        }))
        .unwrap();
    assert!(matches!(
        recv_broker(&mut probe),
        BrokerMsg::Status(Status {
            state: LaunchState::Unknown,
            ..
        })
    ));
    h.stop();
}
