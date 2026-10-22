// SPDX-License-Identifier: MIT OR Apache-2.0
//! Client-side launch state machine: pure event → command mapping.
//!
//! No I/O, no clock — timeouts are the transport's job; the machine only
//! reacts to events. `on_event` returns 0..=2 commands per event.
//!
//! Central safety rule (docs/stages/m11-warm-start.md, «Принятие запроса и
//! жизненный цикл broker»): a decline BEFORE acceptance means cold fallback
//! is safe. AMBIGUITY (Launch sent, no ack, connection dropped) means the
//! client MUST first `Query` the same request-id. Only `LaunchState::Unknown`
//! (the broker PROVES it never accepted — it never saw the id) allows the
//! cold fallback. `LaunchState::UnknownOutcome` (broker restarted) is
//! reported to the user and NEVER leads to a blind cold duplicate.
//!
//! Transition table:
//!
//! | State       | Event                      | New state       | Commands |
//! |-------------|----------------------------|-----------------|----------|
//! | Connecting  | Connected                  | Handshaking     | SendHello |
//! | Connecting  | ConnectionDropped          | Fallback        | ColdFallback |
//! | Handshaking | HelloAcked (version match) | Ready           | — |
//! | Handshaking | HelloAcked (other version) | Fallback        | ColdFallback |
//! | Handshaking | Rejected / HandshakeFailed / ConnectionDropped | Fallback | ColdFallback |
//! | Ready       | ConnectionDropped / Rejected | Fallback      | ColdFallback (nothing sent) |
//! | Ready       | (queue_launch)             | Sent            | SendLaunch |
//! | Sent        | Acked                      | Done            | Finish |
//! | Sent        | Declined(r)                | Fallback        | ReportDeclined(r), ColdFallback |
//! | Sent        | ConnectionDropped          | Querying        | SendQuery (NEVER immediate fallback) |
//! | Sent        | Rejected                   | Ambiguous       | ReportAmbiguous (protocol violation, fail-closed) |
//! | Querying    | QueryAnswered(Unknown)     | Fallback        | ColdFallback (proof of non-acceptance) |
//! | Querying    | QueryAnswered(Accepted/HandedOff) | Done     | Finish (broker accepted; app lifecycle is outside this machine) |
//! | Querying    | QueryAnswered(Failed(m))   | Fallback        | ReportFailed(m), ColdFallback |
//! | Querying    | QueryAnswered(UnknownOutcome) | UnknownOutcome | ReportUnknownOutcome |
//! | Querying    | ConnectionDropped / Rejected | Ambiguous     | ReportAmbiguous (cannot resolve) |
//!
//! Events invalid for the current state are ignored (empty command list,
//! state unchanged), EXCEPT `ConnectionDropped` in Connecting/Handshaking/
//! Ready where nothing has been sent yet, so the cold path is provably safe.

use super::message::{
    DeclineReason, Launch, LaunchState, Query, RejectReason, RequestId, PROTOCOL_VERSION,
};

/// `Fallback` means the unchanged cold path must run; `ReportAmbiguous` /
/// `ReportUnknownOutcome` map to user-visible messages, never to an
/// automatic second process.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Command {
    SendHello,
    SendLaunch(Launch),
    SendQuery(Query),
    ReportDeclined(DeclineReason),
    ReportFailed(String),
    ColdFallback,
    ReportAmbiguous,
    ReportUnknownOutcome,
    Finish,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum State {
    /// No connection yet.
    Connecting,
    /// Connected, hello in flight.
    Handshaking,
    /// Hello acked; idle, may accept a launch.
    Ready,
    /// Launch sent, no ack yet.
    Sent,
    /// Ambiguity resolved by querying the request-id.
    Querying,
    /// Accepted and (re-)confirmed; terminal.
    Done,
    /// Safe cold path; terminal.
    Fallback,
    /// Could not resolve; report to the user; terminal.
    Ambiguous,
    /// Broker says the outcome is undefined; report to the user, never
    /// re-launch; terminal.
    UnknownOutcome,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Event {
    Connected,
    /// Transport parsed the broker's hello ack; the machine checks only the
    /// protocol version it speaks — build matching is the broker's job.
    HelloAcked {
        protocol_version: u32,
        build: String,
    },
    Rejected(RejectReason),
    Acked,
    Declined(DeclineReason),
    ConnectionDropped,
    QueryAnswered(LaunchState),
    HandshakeFailed,
}

/// The launch was queued while the machine was not `Ready`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct NotReady;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClientLaunch {
    state: State,
    pending: Option<Launch>,
    request_id: RequestId,
}

impl ClientLaunch {
    pub fn new(request_id: RequestId) -> ClientLaunch {
        ClientLaunch {
            state: State::Connecting,
            pending: None,
            request_id,
        }
    }

    pub fn state(&self) -> &State {
        &self.state
    }

    pub fn pending_launch(&self) -> Option<&Launch> {
        self.pending.as_ref()
    }

    /// Queue a launch; only legal in `Ready`. Moves the machine to `Sent`
    /// and returns the `SendLaunch` command directly (no separate
    /// `LaunchQueued` event).
    pub fn queue_launch(&mut self, launch: Launch) -> Result<Command, NotReady> {
        if self.state != State::Ready {
            return Err(NotReady);
        }
        self.pending = Some(launch.clone());
        self.state = State::Sent;
        Ok(Command::SendLaunch(launch))
    }

    pub fn on_event(&mut self, ev: Event) -> Vec<Command> {
        use Command::*;
        use State::*;

        // Terminal states absorb everything.
        if matches!(self.state, Done | Fallback | Ambiguous | UnknownOutcome) {
            return Vec::new();
        }
        // Nothing sent yet in these states: a drop (or hello rejection) is
        // provably safe to resolve cold.
        if matches!(
            (&self.state, &ev),
            (Connecting, Event::ConnectionDropped)
                | (Handshaking, Event::ConnectionDropped)
                | (Handshaking, Event::Rejected(_))
                | (Handshaking, Event::HandshakeFailed)
                | (Ready, Event::ConnectionDropped)
                | (Ready, Event::Rejected(_))
        ) {
            self.state = Fallback;
            return vec![ColdFallback];
        }
        match (&self.state, ev) {
            (Connecting, Event::Connected) => {
                self.state = Handshaking;
                vec![SendHello]
            }
            (
                Handshaking,
                Event::HelloAcked {
                    protocol_version, ..
                },
            ) => {
                if protocol_version == PROTOCOL_VERSION {
                    self.state = Ready;
                    Vec::new()
                } else {
                    self.state = Fallback;
                    vec![ColdFallback]
                }
            }
            (Sent, Event::Acked) => {
                self.state = Done;
                vec![Finish]
            }
            (Sent, Event::Declined(reason)) => {
                self.state = Fallback;
                vec![ReportDeclined(reason), ColdFallback]
            }
            (Sent, Event::ConnectionDropped) => {
                self.state = Querying;
                vec![SendQuery(Query {
                    request_id: self.request_id.clone(),
                })]
            }
            (Sent, Event::Rejected(_)) => {
                // Protocol violation after the launch was sent: fail-closed.
                self.state = Ambiguous;
                vec![ReportAmbiguous]
            }
            (Querying, Event::QueryAnswered(LaunchState::Unknown)) => {
                self.state = Fallback;
                vec![ColdFallback]
            }
            (Querying, Event::QueryAnswered(state))
                if matches!(state, LaunchState::Accepted | LaunchState::HandedOff) =>
            {
                self.state = Done;
                vec![Finish]
            }
            (Querying, Event::QueryAnswered(LaunchState::Failed(reason))) => {
                self.state = Fallback;
                vec![ReportFailed(reason), ColdFallback]
            }
            (Querying, Event::QueryAnswered(LaunchState::UnknownOutcome)) => {
                self.state = UnknownOutcome;
                vec![ReportUnknownOutcome]
            }
            (Querying, Event::ConnectionDropped) | (Querying, Event::Rejected(_)) => {
                self.state = Ambiguous;
                vec![ReportAmbiguous]
            }
            // Everything else is invalid for the current state: ignore.
            _ => Vec::new(),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn launch(id: &str) -> Launch {
        Launch {
            request_id: id.into(),
            target: crate::protocol::message::OsBytes::Unix(b"/apps/demo".to_vec()),
            args: Vec::new(),
            cwd: None,
            env: Vec::new(),
            console: false,
        }
    }

    fn ready() -> ClientLaunch {
        let mut c = ClientLaunch::new("r1".into());
        assert_eq!(c.on_event(Event::Connected), vec![Command::SendHello]);
        assert_eq!(c.state(), &State::Handshaking);
        assert_eq!(
            c.on_event(Event::HelloAcked {
                protocol_version: PROTOCOL_VERSION,
                build: "b".into()
            }),
            Vec::<Command>::new()
        );
        assert_eq!(c.state(), &State::Ready);
        c
    }

    #[test]
    fn hello_path_reaches_ready() {
        ready();
    }

    #[test]
    fn hello_wrong_version_falls_back() {
        let mut c = ClientLaunch::new("r1".into());
        assert_eq!(c.on_event(Event::Connected), vec![Command::SendHello]);
        assert_eq!(
            c.on_event(Event::HelloAcked {
                protocol_version: 99,
                build: "b".into()
            }),
            vec![Command::ColdFallback]
        );
        assert_eq!(c.state(), &State::Fallback);
    }

    #[test]
    fn hello_rejected_falls_back() {
        let mut c = ClientLaunch::new("r1".into());
        assert_eq!(c.on_event(Event::Connected), vec![Command::SendHello]);
        assert_eq!(
            c.on_event(Event::Rejected(RejectReason::IncompatibleBuild)),
            vec![Command::ColdFallback]
        );
        assert_eq!(c.state(), &State::Fallback);
    }

    #[test]
    fn handshake_failed_falls_back() {
        let mut c = ClientLaunch::new("r1".into());
        assert_eq!(c.on_event(Event::Connected), vec![Command::SendHello]);
        assert_eq!(
            c.on_event(Event::HandshakeFailed),
            vec![Command::ColdFallback]
        );
        assert_eq!(c.state(), &State::Fallback);
    }

    #[test]
    fn drop_during_handshake_falls_back() {
        let mut c = ClientLaunch::new("r1".into());
        assert_eq!(c.on_event(Event::Connected), vec![Command::SendHello]);
        assert_eq!(
            c.on_event(Event::ConnectionDropped),
            vec![Command::ColdFallback]
        );
        assert_eq!(c.state(), &State::Fallback);
    }

    #[test]
    fn drop_while_connecting_falls_back() {
        let mut c = ClientLaunch::new("r1".into());
        assert_eq!(
            c.on_event(Event::ConnectionDropped),
            vec![Command::ColdFallback]
        );
        assert_eq!(c.state(), &State::Fallback);
    }

    #[test]
    fn drop_while_ready_is_safe_fallback() {
        let mut c = ready();
        assert_eq!(
            c.on_event(Event::ConnectionDropped),
            vec![Command::ColdFallback]
        );
        assert_eq!(c.state(), &State::Fallback);
    }

    #[test]
    fn queue_in_ready_moves_to_sent_with_send_launch() {
        let mut c = ready();
        let cmd = c.queue_launch(launch("r1")).unwrap();
        assert_eq!(cmd, Command::SendLaunch(launch("r1")));
        assert_eq!(c.state(), &State::Sent);
        assert_eq!(c.pending_launch(), Some(&launch("r1")));
    }

    #[test]
    fn queue_outside_ready_is_err() {
        let mut c = ClientLaunch::new("r1".into());
        assert_eq!(c.queue_launch(launch("r1")), Err(NotReady));
        assert_eq!(c.state(), &State::Connecting);
    }

    #[test]
    fn ack_finishes_done() {
        let mut c = ready();
        c.queue_launch(launch("r1")).unwrap();
        assert_eq!(c.on_event(Event::Acked), vec![Command::Finish]);
        assert_eq!(c.state(), &State::Done);
    }

    #[test]
    fn declined_reports_then_falls_back() {
        let mut c = ready();
        c.queue_launch(launch("r1")).unwrap();
        assert_eq!(
            c.on_event(Event::Declined(DeclineReason::NoSpare)),
            vec![
                Command::ReportDeclined(DeclineReason::NoSpare),
                Command::ColdFallback
            ]
        );
        assert_eq!(c.state(), &State::Fallback);
    }

    #[test]
    fn drop_in_sent_sends_query_never_fallback() {
        let mut c = ready();
        c.queue_launch(launch("r1")).unwrap();
        assert_eq!(
            c.on_event(Event::ConnectionDropped),
            vec![Command::SendQuery(Query {
                request_id: "r1".into()
            })]
        );
        assert_eq!(c.state(), &State::Querying);
    }

    #[test]
    fn query_unknown_proves_non_acceptance_falls_back() {
        let mut c = ready();
        c.queue_launch(launch("r1")).unwrap();
        assert_eq!(
            c.on_event(Event::ConnectionDropped),
            vec![Command::SendQuery(Query {
                request_id: "r1".into()
            })]
        );
        assert_eq!(
            c.on_event(Event::QueryAnswered(LaunchState::Unknown)),
            vec![Command::ColdFallback]
        );
        assert_eq!(c.state(), &State::Fallback);
    }

    #[test]
    fn query_accepted_means_done() {
        for state in [LaunchState::Accepted, LaunchState::HandedOff] {
            let mut c = ready();
            c.queue_launch(launch("r1")).unwrap();
            c.on_event(Event::ConnectionDropped);
            assert_eq!(
                c.on_event(Event::QueryAnswered(state)),
                vec![Command::Finish]
            );
            assert_eq!(c.state(), &State::Done);
        }
    }

    #[test]
    fn query_failed_reports_then_falls_back() {
        let mut c = ready();
        c.queue_launch(launch("r1")).unwrap();
        c.on_event(Event::ConnectionDropped);
        assert_eq!(
            c.on_event(Event::QueryAnswered(LaunchState::Failed("bind".into()))),
            vec![Command::ReportFailed("bind".into()), Command::ColdFallback]
        );
        assert_eq!(c.state(), &State::Fallback);
    }

    #[test]
    fn query_unknown_outcome_reports_never_relaunches() {
        let mut c = ready();
        c.queue_launch(launch("r1")).unwrap();
        c.on_event(Event::ConnectionDropped);
        let cmds = c.on_event(Event::QueryAnswered(LaunchState::UnknownOutcome));
        assert_eq!(cmds, vec![Command::ReportUnknownOutcome]);
        assert!(!cmds.contains(&Command::ColdFallback)); // never a blind cold duplicate
        assert_eq!(c.state(), &State::UnknownOutcome);
    }

    #[test]
    fn second_drop_in_querying_is_ambiguous() {
        let mut c = ready();
        c.queue_launch(launch("r1")).unwrap();
        c.on_event(Event::ConnectionDropped);
        assert_eq!(
            c.on_event(Event::ConnectionDropped),
            vec![Command::ReportAmbiguous]
        );
        assert_eq!(c.state(), &State::Ambiguous);
    }

    #[test]
    fn rejected_in_sent_is_protocol_violation_ambiguous() {
        let mut c = ready();
        c.queue_launch(launch("r1")).unwrap();
        assert_eq!(
            c.on_event(Event::Rejected(RejectReason::BadRequest)),
            vec![Command::ReportAmbiguous]
        );
        assert_eq!(c.state(), &State::Ambiguous);
    }

    #[test]
    fn rejected_in_querying_is_ambiguous() {
        let mut c = ready();
        c.queue_launch(launch("r1")).unwrap();
        c.on_event(Event::ConnectionDropped);
        assert_eq!(
            c.on_event(Event::Rejected(RejectReason::BadRequest)),
            vec![Command::ReportAmbiguous]
        );
        assert_eq!(c.state(), &State::Ambiguous);
    }

    #[test]
    fn invalid_events_are_ignored() {
        // Ack before sending anything, Connected twice, Declined in Ready.
        let mut c = ready();
        assert_eq!(c.on_event(Event::Acked), Vec::<Command>::new());
        assert_eq!(c.on_event(Event::Connected), Vec::<Command>::new());
        assert_eq!(c.state(), &State::Ready);
        assert_eq!(
            c.on_event(Event::Declined(DeclineReason::Busy)),
            Vec::<Command>::new()
        );
        assert_eq!(c.state(), &State::Ready);
    }

    #[test]
    fn terminal_states_absorb_everything() {
        for (mut c, ev) in [
            {
                let mut c = ready();
                c.queue_launch(launch("r1")).unwrap();
                c.on_event(Event::Acked);
                (c, Event::ConnectionDropped)
            },
            {
                let mut c = ready();
                c.queue_launch(launch("r1")).unwrap();
                c.on_event(Event::Declined(DeclineReason::Busy));
                (c, Event::Acked)
            },
        ] {
            let before = c.state().clone();
            assert_eq!(c.on_event(ev), Vec::<Command>::new());
            assert_eq!(c.state(), &before);
        }
    }

    #[test]
    fn state_accessors_initial() {
        let c = ClientLaunch::new("r1".into());
        assert_eq!(c.state(), &State::Connecting);
        assert_eq!(c.pending_launch(), None);
    }
}
