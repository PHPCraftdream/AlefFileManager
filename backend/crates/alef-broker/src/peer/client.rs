// SPDX-License-Identifier: MIT OR Apache-2.0
//! Library for `alef run` as a warm client (docs/stages/m11-warm-start.md,
//! «Архитектура реализации» and «Принятие запроса и жизненный цикл broker»):
//! send one `Launch`, relay the spare's output to the caller's streams until
//! `Exited`, and map every failure to either a PROVEN decline (the unchanged
//! cold path is safe) or an AMBIGUOUS outcome (never a blind second launch).
//!
//! Ambiguity resolution follows `protocol::client`'s state machine semantics:
//! after a lost/absent ack the SAME request-id is sent as `Query` — first on
//! the still-open connection (read-timeout case), otherwise on exactly one
//! fresh connection (broken-connection case). `Status::Unknown` (proof of
//! non-acceptance) → cold; `Accepted`/`HandedOff`/`UnknownOutcome` → the
//! launch belongs to the broker, report it and never duplicate.

use std::io::{self, ErrorKind, Write};
use std::time::Duration;

use crate::ipc::endpoint::Endpoint;
use crate::ipc::token::load_or_create_token;
use crate::ipc::{Client, ConnectError};
use crate::protocol::message::{DeclineReason, Launch, LaunchState, Query, Status};
use crate::protocol::warm::{decode_warm_relay_msg, OutputStream, WarmRelayMsg};

use super::own_hello;

/// Wait for the `Accepted`/`Declined` ack after `Launch`.
pub const ACK_TIMEOUT: Duration = Duration::from_secs(5);
/// Wait for a `Status` answer to a `Query`.
pub const QUERY_TIMEOUT: Duration = Duration::from_secs(5);
/// Maximum silence between relay frames while the spare runs. Reset on every
/// `Output`/`Exited` frame; a live-but-silent-for-longer app is reported
/// [`Outcome::Ambiguous`] (never duplicated). Tunable by the wiring.
pub const RELAY_IDLE_TIMEOUT: Duration = Duration::from_secs(300);

/// Result of a warm launch attempt.
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Outcome {
    /// The spare ran to completion; `alef run` exits with this code.
    Handled { exit_code: i32 },
    /// A PROVEN decline before acceptance: run the unchanged cold path.
    Cold(ColdReason),
    /// The launch may or may not be running and this client lost the relay
    /// (connection drop, unresolved query, relay idle cap). USER-FACING:
    /// report that the launch state is undetermined and let the user check
    /// or retry deliberately — this library never spawns a second process.
    Ambiguous,
}

/// A proven reason the warm path gave up before acceptance (cold is safe).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ColdReason {
    /// No broker is listening at the entry point.
    NoBroker,
    /// Broker refused the connection as incompatible (protocol/build/store).
    Incompatible,
    /// Mutual authentication failed.
    AuthFailed,
    /// The broker declined the launch before acceptance.
    Declined(DeclineReason),
    /// The pool had no spare of the required kind.
    NoSpare,
    /// Request validation failed (e.g. store-env disagreement).
    ValidationFailed,
    /// The broker reported a post-acceptance failure for this request id.
    Failed(String),
    /// A `Query` proved the broker never saw this request id: nothing was
    /// ever accepted, so the cold path runs without duplication risk.
    QueryUnknown,
    /// Local I/O failure before anything was sent (token, connect).
    Io(String),
}

/// Try a warm launch. Writes the spare's relayed stdout/stderr to the given
/// streams and returns the spare's exit code, or a cold/ambiguous outcome.
pub fn try_launch(
    endpoint: &Endpoint,
    request: &Launch,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> Outcome {
    let mut client = match open_client(endpoint) {
        Ok(c) => c,
        Err(reason) => return Outcome::Cold(reason),
    };
    // Nothing is irreversible until the ack: a send failure here is handled
    // by the query resolution (the frame may or may not have left).
    if client
        .send(&crate::protocol::message::ClientMsg::Launch(
            request.clone(),
        ))
        .is_err()
    {
        return resolve(endpoint, Some(client), &request.request_id);
    }
    match wait_ack(&mut client, &request.request_id) {
        Ack::Accepted => relay(endpoint, client, &request.request_id, stdout, stderr),
        Ack::Declined(reason) => Outcome::Cold(cold_from_decline(reason)),
        Ack::Lost { same_conn } => {
            let client = if same_conn { Some(client) } else { None };
            resolve(endpoint, client, &request.request_id)
        }
    }
}

enum Ack {
    Accepted,
    Declined(DeclineReason),
    /// No ack within [`ACK_TIMEOUT`] (`same_conn = true`: the connection is
    /// still usable for a `Query`) or the connection dropped (`false`).
    Lost {
        same_conn: bool,
    },
}

fn wait_ack(client: &mut Client, request_id: &str) -> Ack {
    let _ = client.set_read_timeout(Some(ACK_TIMEOUT));
    loop {
        match client.recv() {
            Ok(crate::protocol::message::BrokerMsg::Accepted(a)) if a.request_id == request_id => {
                return Ack::Accepted
            }
            Ok(crate::protocol::message::BrokerMsg::Declined(d)) if d.request_id == request_id => {
                return Ack::Declined(d.reason)
            }
            // Unrelated frame on this connection (not expected): ignore.
            Ok(_) => continue,
            Err(ConnectError::Io(e)) if is_timeout(&e) => return Ack::Lost { same_conn: true },
            Err(_) => return Ack::Lost { same_conn: false },
        }
    }
}

/// Relay loop: `Output` → the matching stream, `Exited` → done. Any loss
/// mid-flight goes through the same query resolution as a lost ack.
fn relay(
    endpoint: &Endpoint,
    mut client: Client,
    request_id: &str,
    stdout: &mut dyn Write,
    stderr: &mut dyn Write,
) -> Outcome {
    let _ = client.set_read_timeout(Some(RELAY_IDLE_TIMEOUT));
    loop {
        match client.recv_raw() {
            Ok(body) => match decode_warm_relay_msg(&body) {
                Ok(WarmRelayMsg::Output(o)) if o.request_id == request_id => {
                    let target: &mut dyn Write = match o.stream {
                        OutputStream::Stdout => stdout,
                        OutputStream::Stderr => stderr,
                    };
                    // A broken local stream means the user sees nothing:
                    // report ambiguity instead of pretending success.
                    if target
                        .write_all(&o.bytes)
                        .and_then(|()| target.flush())
                        .is_err()
                    {
                        return Outcome::Ambiguous;
                    }
                }
                Ok(WarmRelayMsg::Exited(e)) if e.request_id == request_id => {
                    let _ = stdout.flush();
                    let _ = stderr.flush();
                    return Outcome::Handled { exit_code: e.code };
                }
                // Frames for other requests or undecodable bodies are not
                // part of this launch's relay contract: fail closed.
                _ => return Outcome::Ambiguous,
            },
            Err(e) if is_timeout(&e) => {
                // RELAY_IDLE_TIMEOUT elapsed: the process may still run; the
                // query cannot tell (the broker already acked), so report.
                return Outcome::Ambiguous;
            }
            Err(_) => {
                return resolve(endpoint, None, request_id);
            }
        }
    }
}

/// Resolve an ambiguous launch by querying the SAME request id. Tries the
/// still-open connection first, then exactly one fresh connection. A `Query`
/// is idempotent bookkeeping on the broker; a second `Launch` is NEVER sent.
fn resolve(endpoint: &Endpoint, client: Option<Client>, request_id: &str) -> Outcome {
    if let Some(mut c) = client {
        if let Some(state) = query_once(&mut c, request_id) {
            return state_outcome(state);
        }
    }
    match open_client(endpoint) {
        Ok(mut c) => match query_once(&mut c, request_id) {
            Some(state) => state_outcome(state),
            None => Outcome::Ambiguous,
        },
        // No broker NOW is not proof of non-acceptance: the broker may have
        // died after acking (docs: never a blind cold duplicate).
        Err(_) => Outcome::Ambiguous,
    }
}

/// One `Query` exchange on an established connection; `None` = unresolved.
fn query_once(client: &mut Client, request_id: &str) -> Option<LaunchState> {
    let _ = client.set_read_timeout(Some(QUERY_TIMEOUT));
    client
        .send(&crate::protocol::message::ClientMsg::Query(Query {
            request_id: request_id.to_string(),
        }))
        .ok()?;
    loop {
        match client.recv() {
            Ok(crate::protocol::message::BrokerMsg::Status(Status {
                request_id: rid,
                state,
            })) if rid == request_id => return Some(state),
            Ok(_) => continue,
            Err(_) => return None,
        }
    }
}

fn state_outcome(state: LaunchState) -> Outcome {
    match state {
        // Proof of non-acceptance: the cold path is safe.
        LaunchState::Unknown => Outcome::Cold(ColdReason::QueryUnknown),
        LaunchState::Failed(m) => Outcome::Cold(ColdReason::Failed(m)),
        // Accepted/HandedOff: the launch IS the broker's; UnknownOutcome:
        // the broker cannot say. Both are user-facing ambiguity.
        _ => Outcome::Ambiguous,
    }
}

fn cold_from_decline(reason: DeclineReason) -> ColdReason {
    match reason {
        DeclineReason::NoSpare => ColdReason::NoSpare,
        DeclineReason::ValidationFailed => ColdReason::ValidationFailed,
        other => ColdReason::Declined(other),
    }
}

/// Token load (documented path rules) + connect + `Hello`.
fn open_client(endpoint: &Endpoint) -> Result<Client, ColdReason> {
    let (token, _created) =
        load_or_create_token(endpoint).map_err(|e| ColdReason::Io(e.to_string()))?;
    match Client::connect_with_hello(endpoint, &token, own_hello()) {
        Ok(c) => Ok(c),
        Err(ConnectError::NoBroker) => Err(ColdReason::NoBroker),
        Err(ConnectError::Incompatible(_)) => Err(ColdReason::Incompatible),
        Err(ConnectError::AuthFailed) => Err(ColdReason::AuthFailed),
        Err(ConnectError::Io(e)) => Err(ColdReason::Io(e.to_string())),
    }
}

fn is_timeout(e: &io::Error) -> bool {
    matches!(e.kind(), ErrorKind::TimedOut | ErrorKind::WouldBlock)
}
