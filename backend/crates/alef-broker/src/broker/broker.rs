// SPDX-License-Identifier: MIT OR Apache-2.0
//! Broker core (docs/stages/m11-warm-start.md, «Архитектура реализации»):
//! owns the [`pool::Pool`], executes its [`pool::Action`]s through a
//! [`Spawner`], and implements [`ipc::Handler`] so ONE [`ipc::Server`] serves
//! clients, spares and admin on the same authenticated endpoint.
//!
//! Frame routing: the transport decodes post-handshake frames as `ClientMsg`;
//! anything else goes to [`Broker::on_raw_frame`], which decodes
//! `SpareToBroker` (spare role) or `WarmAdminMsg` (admin role). Spare and
//! admin connections are distinguished by their frame bodies; the transport's
//! per-connection id (`conn_id`) correlates a spare's `SpareHello` with its
//! later `Ready`/`Started`/`Declined` frames, which carry no identity of their
//! own. Replies that are not `BrokerMsg` (spare `BrokerToSpare`, admin
//! `WarmAdminReply`, relay `WarmRelayMsg`) are written as raw frames via
//! `ReplyTo::send_raw` / `ConnWriter::send_raw`.
//!
//! Acceptance point: a granted launch stays PENDING (see [`super::pending`])
//! — the client is answered `Accepted` only after the spare reports
//! `SpareToBroker::Started`; a decline, the spare's death or the assign
//! timeout before that answer `Declined` instead.
//!
//! Driving: the pool is pure, so the owner (tray/CLI wiring, or tests with a
//! fake clock) calls [`Broker::pump`] — it polls owned spares for exit,
//! retires them via `pool.on_exited`, runs `pool.fill(now)` and executes all
//! resulting actions, then returns `pool.next_wake()`. The only threads the
//! broker creates are per-launch output relays (EOF-driven, backpressure via
//! the shared client-connection lock) — nothing on its own schedule.

use std::collections::HashMap;
use std::io::{self, Read};
use std::sync::mpsc::{Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use crate::ipc::{ConnWriter, Handler, ReplyTo};
use crate::pool::{self, Action, Clock, Jitter, Pool, PoolConfig, Take};
use crate::protocol::log::{Decision, RequestLog};
use crate::protocol::message::{
    Accepted, BrokerMsg, ClientMsg, DeclineReason, Declined, Launch, LaunchState, OsBytes,
    RequestId, Status, UnsupportedReason,
};
use crate::protocol::warm::{
    decode_spare_to_broker, decode_warm_admin_msg, encode_broker_to_spare, encode_warm_admin_reply,
    encode_warm_relay_msg, AdminAck, AdminStatusReply, Assign, BrokerToSpare, Exited, Output,
    Shutdown, SlotStateEntry, SpareAck, SpareToBroker, Started, WarmAdminMsg, WarmAdminReply,
    MAX_OUTPUT_CHUNK,
};

use super::pending::{PendingLaunch, ASSIGN_TIMEOUT};
use super::spawner::{from_wire_kind, wire_kind, SpareId, SpareProcess, Spawner};

/// Every first-version client launches a windowed app (doc: «безоконный слот
/// пула нужен лишь безоконным приложениям без консоли и по замеру почти не
/// ускоряет»); the broker decides the kind, never the client.
const REQUEST_KIND: pool::SlotKind = pool::SlotKind::Windowed;

/// Poll interval of the exit checks (`pump` for pool spares, the relay
/// finisher for granted processes). Small, fake-friendly: fakes flip a flag
/// and tests wait on channels with timeouts, never sleeps.
const EXIT_POLL: Duration = Duration::from_millis(2);

/// A spare the pool owns (spawned, not yet granted).
struct SpareRecord {
    kind: pool::SlotKind,
    pid: u32,
    /// `None` once granted (the process moved into a [`LaunchRun`]).
    process: Option<Box<dyn SpareProcess>>,
    /// `true` after the spare sent `SpareToBroker::Ready`.
    ready: bool,
    /// Raw-frame writer to the spare's control connection, captured at
    /// `SpareHello` via `ReplyTo::detach`.
    control: Option<Box<dyn ConnWriter + Send>>,
}

/// A granted launch's relay: the finisher thread owns the process and the
/// detached client writer (see [`start_relay`]).

struct Inner<S: Spawner, C: Clock, J: Jitter> {
    pool: Pool<C, J>,
    spawner: Arc<S>,
    /// SpareId → pool-owned spare record.
    spares: HashMap<SpareId, SpareRecord>,
    /// Transport conn_id → SpareId (spare control connections).
    spare_conns: HashMap<u64, SpareId>,
    /// Granted launches with live relay threads (their finisher owns the
    /// process; the broker only counts them).
    log: RequestLog,
    /// Granted but not yet accepted launches (the spare has `Assign`, the
    /// client has no answer yet).
    pending: HashMap<RequestId, PendingLaunch>,
    /// How long a pending launch may wait for the spare's `Started`.
    assign_timeout: Duration,
    /// Broker's own store identity captured at construction (exact values or
    /// absence); a client's `Launch.env` must not disagree with it. (The
    /// `Hello` handshake checks the same identity at connection time.)
    own_alef_home: Option<OsBytes>,
    own_alef_app_home: Option<OsBytes>,
    /// Cheap mirror of pool slot states for `AdminStatusReply` (the pool
    /// itself exposes no state getter).
    slot_view: [String; 2],
    granted_total: u64,
    live_launches: u32,
    shutting_down: bool,
    /// Relay finishers report exactly one exit per granted launch.
    finished_tx: Sender<(RequestId, i32, Option<i32>)>,
    finished_rx: Receiver<(RequestId, i32, Option<i32>)>,
}

/// The broker. The owner starts an `ipc::Server` with `Arc<Broker>` as the
/// `Handler` and calls [`Broker::pump`] from its own loop.
pub struct Broker<S: Spawner + Send + Sync, C: Clock + Send, J: Jitter + Send> {
    inner: Mutex<Inner<S, C, J>>,
    version: String,
}

impl<S: Spawner + Send + Sync, C: Clock + Send, J: Jitter + Send> Broker<S, C, J> {
    pub fn new(
        cfg: PoolConfig,
        spawner: S,
        clock: C,
        jitter: J,
        version: String,
    ) -> Broker<S, C, J> {
        let (finished_tx, finished_rx) = std::sync::mpsc::channel();
        // Exact values INCLUDING absence (doc: absence is a value).
        let own_alef_home = std::env::var_os("ALEF_HOME").map(|v| OsBytes::from_os_string(&v));
        let own_alef_app_home =
            std::env::var_os("ALEF_APP_HOME").map(|v| OsBytes::from_os_string(&v));
        Broker {
            inner: Mutex::new(Inner {
                pool: Pool::new(cfg, clock, jitter),
                spawner: Arc::new(spawner),
                spares: HashMap::new(),
                spare_conns: HashMap::new(),
                pending: HashMap::new(),
                assign_timeout: ASSIGN_TIMEOUT,
                live_launches: 0,
                log: RequestLog::new(1024, 3_600_000),
                own_alef_home,
                own_alef_app_home,
                slot_view: ["empty".into(), "empty".into()],
                granted_total: 0,
                shutting_down: false,
                finished_tx,
                finished_rx,
            }),
            version,
        }
    }

    /// Owner entry point: retire exited pool spares, collect finished launch
    /// exits, run `fill` and execute the resulting actions. Returns the pool's
    /// `next_wake` (relative to the pool clock's zero) or `None`.
    pub fn pump(&self) -> Option<Duration> {
        let mut inner = self.lock();
        let now = inner.pool.clock().now();

        // Pool-owned spares that died before a grant.
        let mut exited = Vec::new();
        for (id, rec) in inner.spares.iter_mut() {
            let Some(proc) = rec.process.as_mut() else {
                continue;
            };
            if matches!(proc.try_wait(), Ok(Some(_))) {
                exited.push((*id, rec.kind));
            }
        }
        for (id, kind) in exited {
            inner.spares.remove(&id);
            let conns: Vec<u64> = inner
                .spare_conns
                .iter()
                .filter(|(_, sid)| **sid == id)
                .map(|(cid, _)| *cid)
                .collect();
            for cid in conns {
                inner.spare_conns.remove(&cid);
            }
            let _ = inner.pool.on_exited(id, now);
            inner.slot_view[kind.index()] = "backoff".into();
        }

        // Pending launches whose spare died or went silent past the assign
        // deadline BEFORE `Started`: the client (and any duplicate writers)
        // was never answered — decline now.
        let mut lost: Vec<(RequestId, SpareId, bool)> = Vec::new(); // bool = timed out
        let assign_timeout = inner.assign_timeout;
        for (rid, p) in inner.pending.iter_mut() {
            let dead = matches!(p.process.try_wait(), Ok(Some(_)));
            let late = now.saturating_sub(p.since) > assign_timeout;
            if dead || late {
                lost.push((rid.clone(), p.spare_id, late));
            }
        }
        for (rid, id, late) in lost {
            let Some(mut p) = inner.pending.remove(&rid) else {
                continue;
            };
            p.process.kill(); // no-op if already dead
            let reason = if late {
                DeclineReason::ValidationFailed
            } else {
                DeclineReason::NoSpare
            };
            let _ = inner.pool.on_exited(id, now);
            inner.slot_view[REQUEST_KIND.index()] = "backoff".into();
            inner.log.decide(
                &rid,
                Decision::Declined(reason.clone()),
                now.as_millis() as u64,
            );
            let msg = BrokerMsg::Declined(Declined {
                request_id: rid,
                reason,
            });
            let _ = p.writer.send(&msg);
            for mut w in p.extra.drain(..) {
                let _ = w.send(&msg);
            }
        }

        // Granted launches whose process exited (their finisher thread relayed
        // `Exited` to the client and reports here for bookkeeping).
        while let Ok((rid, _code, _signal)) = inner.finished_rx.try_recv() {
            inner.live_launches = inner.live_launches.saturating_sub(1);
            let _ = inner.log.handoff(&rid, now.as_millis() as u64);
        }

        let actions = inner.pool.fill(now);
        inner.execute(&actions);
        inner.pool.next_wake()
    }

    /// «Выход»: stop serving warm launches, kill every spare the pool still
    /// owns, and KEEP granted/running applications untouched (their relay
    /// threads continue until `Exited`).
    pub fn shutdown(&self) {
        let mut inner = self.lock();
        inner.shutting_down = true;
        let now = inner.pool.clock().now();
        let now_ms = now.as_millis() as u64;
        // Pending launches were granted but never accepted: kill the spare
        // and decline every waiting client (including duplicates).
        let waiting: Vec<_> = inner.pending.drain().collect();
        for (rid, mut p) in waiting {
            p.process.kill();
            let _ = inner
                .log
                .decide(&rid, Decision::Declined(DeclineReason::Busy), now_ms);
            let msg = BrokerMsg::Declined(Declined {
                request_id: p.rid.clone(),
                reason: DeclineReason::Busy,
            });
            let _ = p.writer.send(&msg);
            for mut w in p.extra.drain(..) {
                let _ = w.send(&msg);
            }
        }
        let actions = inner.pool.shutdown();
        inner.execute(&actions);
        let ids: Vec<SpareId> = inner.spares.keys().copied().collect();
        for id in ids {
            if let Some(mut rec) = inner.spares.remove(&id) {
                if let Some(p) = rec.process.as_mut() {
                    p.kill();
                }
            }
        }
        let conns: Vec<u64> = inner.spare_conns.keys().copied().collect();
        for cid in conns {
            inner.spare_conns.remove(&cid);
        }
        inner.slot_view = ["empty".into(), "empty".into()];
    }

    /// Test hook: shrink the pending-launch deadline.
    pub fn set_assign_timeout(&self, d: Duration) {
        self.lock().assign_timeout = d;
    }

    /// Read-only admin snapshot (slot states from the broker's bookkeeping).
    pub fn admin_status(&self) -> AdminStatusReply {
        let inner = self.lock();
        let slots = pool::SlotKind::ALL
            .iter()
            .map(|&k| SlotStateEntry {
                kind: wire_kind(k),
                state: inner.slot_view[k.index()].clone(),
            })
            .collect();
        AdminStatusReply {
            pid: std::process::id(),
            version: self.version.clone(),
            running: inner.spares.len() as u32 + inner.live_launches,
            granted_total: inner.granted_total,
            slots,
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Inner<S, C, J>> {
        self.inner.lock().unwrap_or_else(|p| p.into_inner())
    }
}

impl<S: Spawner + Send + Sync, C: Clock + Send, J: Jitter + Send> Handler for Broker<S, C, J> {
    fn on_message(&self, conn: &mut dyn ReplyTo, msg: ClientMsg) {
        match msg {
            ClientMsg::Launch(launch) => self.handle_launch(conn, launch),
            ClientMsg::Query(q) => {
                let state = {
                    let mut inner = self.lock();
                    // A pending launch may still be accepted at any moment:
                    // report Accepted so the client does not run the cold
                    // path while the spare completes its pre-start checks.
                    if inner.pending.contains_key(&q.request_id) {
                        LaunchState::Accepted
                    } else {
                        let now_ms = inner.pool.clock().now().as_millis() as u64;
                        inner.log.query(&q.request_id, now_ms)
                    }
                };
                let _ = ReplyTo::send(
                    conn,
                    &BrokerMsg::Status(Status {
                        request_id: q.request_id,
                        state,
                    }),
                );
            }
            // Duplicate Hello is rejected by the transport before dispatch.
            ClientMsg::Hello(_) => {}
        }
    }

    fn on_raw_frame(&self, conn: &mut dyn ReplyTo, conn_id: u64, body: &[u8]) -> io::Result<()> {
        if let Ok(admin) = decode_warm_admin_msg(body) {
            self.handle_admin(conn, admin);
            return Ok(());
        }
        match decode_spare_to_broker(body) {
            Ok(spare) => {
                self.handle_spare(conn, conn_id, spare);
                Ok(())
            }
            Err(_) => Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "frame is neither spare nor admin",
            )),
        }
    }
}

impl<S: Spawner + Send + Sync, C: Clock + Send, J: Jitter + Send> Broker<S, C, J> {
    fn handle_launch(&self, conn: &mut dyn ReplyTo, launch: Launch) {
        let rid = launch.request_id.clone();
        let mut inner = self.lock();
        let now = inner.pool.clock().now();
        let now_ms = now.as_millis() as u64;

        // Duplicate Launch while a spare holds this request: attach another
        // writer to share the outcome — no answer now, no second spawn.
        if let Some(pending) = inner.pending.get_mut(&rid) {
            if let Ok(w) = conn.detach() {
                pending.extra.push(w);
            }
            return;
        }

        // Idempotency BEFORE any irreversible action (RequestLog contract):
        // a repeated Launch replays the recorded decision, never takes a
        // second spare.
        match inner.log.query(&rid, now_ms) {
            LaunchState::Unknown => {}
            LaunchState::Accepted | LaunchState::HandedOff => {
                let _ = ReplyTo::send(conn, &BrokerMsg::Accepted(Accepted { request_id: rid }));
                return;
            }
            LaunchState::Failed(_) => {
                // A recorded terminal decline; the wire replays it as a
                // decline (the exact reason text lives in the log only).
                let _ = ReplyTo::send(
                    conn,
                    &BrokerMsg::Declined(Declined {
                        request_id: rid,
                        reason: DeclineReason::ValidationFailed,
                    }),
                );
                return;
            }
            LaunchState::UnknownOutcome => unreachable!("a live broker log has no UnknownOutcome"),
        }

        // 1. Validation — every failure declines BEFORE any irreversible
        //    action («Что в первой версии идёт мимо прогрева»).
        if let Some(reason) = validate(&launch, &inner) {
            inner
                .log
                .decide(&rid, Decision::Declined(reason.clone()), now_ms);
            let _ = ReplyTo::send(
                conn,
                &BrokerMsg::Declined(Declined {
                    request_id: rid,
                    reason,
                }),
            );
            return;
        }
        if inner.shutting_down {
            inner
                .log
                .decide(&rid, Decision::Declined(DeclineReason::Busy), now_ms);
            let _ = ReplyTo::send(
                conn,
                &BrokerMsg::Declined(Declined {
                    request_id: rid,
                    reason: DeclineReason::Busy,
                }),
            );
            return;
        }

        // 2. Pool take: atomic grant + same-kind replacement scheduling.
        let take = inner.pool.take(REQUEST_KIND, now);
        let spare_id = match &take {
            Take::Granted { id, actions } => {
                inner.execute(actions);
                Some(*id)
            }
            Take::Unavailable { actions } => {
                inner.execute(actions);
                None
            }
        };

        // 3. Bind the granted spare to this connection: Assign over the
        //    spare's control writer, detach the client writers for the final
        //    answer and the future relay. The launch stays PENDING.
        let bound = match spare_id {
            Some(id) => bind_grant(&mut inner, conn, id, &launch),
            None => None,
        };

        match bound {
            Some((id, writer, writer_out, writer_err)) => {
                // The spare leaves the pool's bookkeeping entirely: its
                // process moves into the pending entry; acceptance is decided
                // by the spare's `Started` (or its absence).
                let registered = inner
                    .spares
                    .remove(&id)
                    .and_then(|mut rec| rec.process.take());
                match registered {
                    Some(process) => {
                        inner.pending.insert(
                            rid.clone(),
                            PendingLaunch {
                                rid: rid.clone(),
                                spare_id: id,
                                process,
                                writer,
                                writer_out,
                                writer_err,
                                extra: Vec::new(),
                                since: now,
                            },
                        );
                    }
                    None => {
                        // Unreachable in practice (a registered spare always
                        // carries its process); fail closed like the None arm.
                        let _ = inner.pool.on_exited(id, now);
                        inner
                            .log
                            .decide(&rid, Decision::Declined(DeclineReason::NoSpare), now_ms);
                        let _ = ReplyTo::send(
                            conn,
                            &BrokerMsg::Declined(Declined {
                                request_id: rid,
                                reason: DeclineReason::NoSpare,
                            }),
                        );
                    }
                }
            }
            None => {
                // Granted but not deliverable (spare connection gone or
                // not Ready): decline without launching anything. The slot
                // already left Ready; `on_exited` keeps accounting sane.
                if let Some(id) = spare_id {
                    let _ = inner.pool.on_exited(id, now);
                }
                inner
                    .log
                    .decide(&rid, Decision::Declined(DeclineReason::NoSpare), now_ms);
                let _ = ReplyTo::send(
                    conn,
                    &BrokerMsg::Declined(Declined {
                        request_id: rid,
                        reason: DeclineReason::NoSpare,
                    }),
                );
            }
        }
    }

    fn handle_spare(&self, conn: &mut dyn ReplyTo, conn_id: u64, msg: SpareToBroker) {
        let mut inner = self.lock();
        match msg {
            SpareToBroker::Hello(hello) => {
                let kind = from_wire_kind(hello.kind);
                let id = Some(SpareId(hello.spare_id))
                    .filter(|id| inner.spares.get(id).is_some_and(|r| r.kind == kind));
                let Some(id) = id else {
                    // Unknown spare (already granted/killed): tell it to exit.
                    if let Ok(body) = encode_broker_to_spare(&BrokerToSpare::Shutdown(Shutdown)) {
                        let _ = ReplyTo::send_raw(conn, &body);
                    }
                    return;
                };
                match conn.detach() {
                    Ok(control) => {
                        let rec = inner.spares.get_mut(&id).expect("id from live records");
                        rec.control = Some(control);
                        inner.spare_conns.insert(conn_id, id);
                    }
                    Err(_) => return,
                }
                if let Ok(body) = encode_broker_to_spare(&BrokerToSpare::Ack(SpareAck)) {
                    let _ = ReplyTo::send_raw(conn, &body);
                }
            }
            SpareToBroker::Ready(_) => {
                if let Some(&id) = inner.spare_conns.get(&conn_id) {
                    if inner.spares.contains_key(&id) {
                        let now = inner.pool.clock().now();
                        inner.pool.on_ready(id, now);
                        if let Some(rec) = inner.spares.get_mut(&id) {
                            rec.ready = true;
                            let index = rec.kind.index();
                            inner.slot_view[index] = "ready".into();
                        }
                    }
                }
            }
            SpareToBroker::Started(_) => {
                // The acceptance point: every spare-side check passed and the
                // spare begins irreversible actions. Answer the client (and
                // every duplicate) `Accepted`, start the relay, account the
                // launch. A `Started` from a spare with no pending entry is
                // stale — ignore it.
                let Some(&id) = inner.spare_conns.get(&conn_id) else {
                    return;
                };
                let pending_rid = inner
                    .pending
                    .iter()
                    .find(|(_, p)| p.spare_id == id)
                    .map(|(rid, _)| rid.clone());
                let Some(rid) = pending_rid else {
                    return;
                };
                let Some(mut p) = inner.pending.remove(&rid) else {
                    return;
                };
                let now_ms = inner.pool.clock().now().as_millis() as u64;
                inner.log.decide(&rid, Decision::Accepted, now_ms);
                let accepted = BrokerMsg::Accepted(Accepted {
                    request_id: rid.clone(),
                });
                let _ = p.writer.send(&accepted);
                for mut w in p.extra.drain(..) {
                    let _ = w.send(&accepted);
                }
                inner.granted_total += 1;
                inner.live_launches += 1;
                inner.slot_view[REQUEST_KIND.index()] = "initializing".into();
                start_relay(&inner, rid, p.process, p.writer, p.writer_out, p.writer_err);
            }
            SpareToBroker::Declined(declined) => {
                let Some(&id) = inner.spare_conns.get(&conn_id) else {
                    return;
                };
                // Decline AFTER the grant: the spare was assigned a pending
                // launch. Kill the spare, decline the client — the launch is
                // proven dead, the cold path is safe.
                let pending_rid = inner
                    .pending
                    .iter()
                    .find(|(_, p)| p.spare_id == id)
                    .map(|(rid, _)| rid.clone());
                if let Some(rid) = pending_rid {
                    let now = inner.pool.clock().now();
                    let now_ms = now.as_millis() as u64;
                    if let Some(mut p) = inner.pending.remove(&rid) {
                        p.process.kill();
                        // The spare's reason is free text; the wire's
                        // `DeclineReason` has no free-text field.
                        let msg = BrokerMsg::Declined(Declined {
                            request_id: rid.clone(),
                            reason: DeclineReason::ValidationFailed,
                        });
                        let _ = p.writer.send(&msg);
                        for mut w in p.extra.drain(..) {
                            let _ = w.send(&msg);
                        }
                    }
                    inner.spare_conns.remove(&conn_id);
                    let _ = inner.pool.on_failed(id, now);
                    inner.slot_view[REQUEST_KIND.index()] = "backoff".into();
                    inner.log.decide(
                        &rid,
                        Decision::Declined(DeclineReason::ValidationFailed),
                        now_ms,
                    );
                    return;
                }
                // The spare refuses before any irreversible action: retire it.
                // Assign is bound in the same call as the grant, so a spare
                // Declined never refers to a launch we already acked.
                if let Some(mut rec) = inner.spares.remove(&id) {
                    if let Some(p) = rec.process.as_mut() {
                        p.kill();
                    }
                    inner.slot_view[rec.kind.index()] = "backoff".into();
                }
                inner.spare_conns.remove(&conn_id);
                let failed_at = inner.pool.clock().now();
                let _ = inner.pool.on_failed(id, failed_at);
                let _ = declined;
            }
        }
    }

    fn handle_admin(&self, conn: &mut dyn ReplyTo, msg: WarmAdminMsg) {
        let reply = match msg {
            WarmAdminMsg::Status(_) => WarmAdminReply::StatusReply(self.admin_status()),
            WarmAdminMsg::Shutdown(_) => {
                self.shutdown();
                WarmAdminReply::Ack(AdminAck)
            }
        };
        if let Ok(body) = encode_warm_admin_reply(&reply) {
            let _ = ReplyTo::send_raw(conn, &body);
        }
    }
}

/// Send `Assign` to a Ready spare's control writer and detach three client
/// writers (final answer + both relay streams). The launch is now PENDING:
/// the client is answered only when the spare reports `Started`. `None` =
/// not deliverable.
fn bind_grant<S: Spawner, C: Clock, J: Jitter>(
    inner: &mut Inner<S, C, J>,
    conn: &mut dyn ReplyTo,
    id: SpareId,
    launch: &Launch,
) -> Option<(
    SpareId,
    Box<dyn ConnWriter + Send>,
    Box<dyn ConnWriter + Send>,
    Box<dyn ConnWriter + Send>,
)> {
    if !inner.spares.get(&id)?.ready {
        return None;
    }
    let control = inner.spares.get_mut(&id)?.control.as_mut()?;
    let body = encode_broker_to_spare(&BrokerToSpare::Assign(Assign {
        launch: launch.clone(),
    }))
    .ok()?;
    control.send_raw(&body).ok()?;
    let writer = conn.detach().ok()?;
    let writer_out = conn.detach().ok()?;
    let writer_err = conn.detach().ok()?;
    Some((id, writer, writer_out, writer_err))
}

/// Start the output relay: one thread per stream reading to EOF in
/// ≤ MAX_OUTPUT_CHUNK chunks and sending `WarmRelayMsg::Output` through its
/// own detached writer (all writers share the client connection's lock, so
/// frames never interleave and a blocked write backpressures the spare's
/// pipe — the broker stops draining, the OS pipe stalls the process). A
/// finisher thread waits for both streams, waits for the exit, sends EXACTLY
/// ONE `WarmRelayMsg::Exited` and reports the exit through `finished_tx`
/// (drained by `pump` for bookkeeping: live-launch count + `log.handoff`).
fn start_relay<S: Spawner, C: Clock, J: Jitter>(
    inner: &Inner<S, C, J>,
    rid: RequestId,
    mut process: Box<dyn SpareProcess>,
    mut writer: Box<dyn ConnWriter + Send>,
    writer_out: Box<dyn ConnWriter + Send>,
    writer_err: Box<dyn ConnWriter + Send>,
) {
    let stdout = process.stdout();
    let stderr = process.stderr();

    let h_out = stdout.map(|r| {
        let rid = rid.clone();
        let w = writer_out;
        std::thread::Builder::new()
            .name("spare-stdout".into())
            .spawn(move || relay_stream(r, w, rid, true))
            .ok()
    });
    let h_err = stderr.map(|r| {
        let rid = rid.clone();
        let w = writer_err;
        std::thread::Builder::new()
            .name("spare-stderr".into())
            .spawn(move || relay_stream(r, w, rid, false))
            .ok()
    });
    let tx = inner.finished_tx.clone();
    let rid_exit = rid.clone();
    let _ = std::thread::Builder::new()
        .name("spare-finisher".into())
        .spawn(move || {
            for h in [h_out.flatten(), h_err.flatten()].into_iter().flatten() {
                let _ = h.join();
            }
            let mut info: Option<super::spawner::ExitInfo> = None;
            loop {
                match process.try_wait() {
                    Ok(Some(e)) => {
                        info = Some(e);
                        break;
                    }
                    Ok(None) => std::thread::sleep(EXIT_POLL),
                    Err(_) => {
                        info = Some(super::spawner::ExitInfo {
                            code: -1,
                            signal: None,
                        });
                        break;
                    }
                }
            }
            let e = info.unwrap_or(super::spawner::ExitInfo {
                code: -1,
                signal: None,
            });
            let exited = Exited {
                request_id: rid_exit.clone(),
                code: e.code,
                signal: e.signal,
            };
            if let Ok(body) =
                encode_warm_relay_msg(&crate::protocol::warm::WarmRelayMsg::Exited(exited))
            {
                let _ = writer.send_raw(&body);
            }
            let _ = tx.send((rid_exit, e.code, e.signal));
        });
}

/// One stream relay: read to EOF, chunk → `Output`, never buffer unboundedly.
fn relay_stream(
    mut r: Box<dyn Read + Send>,
    mut w: Box<dyn ConnWriter + Send>,
    rid: RequestId,
    stdout: bool,
) {
    let mut buf = vec![0u8; MAX_OUTPUT_CHUNK];
    loop {
        match r.read(&mut buf) {
            Ok(0) => break, // EOF
            Ok(n) => {
                let msg = Output {
                    request_id: rid.clone(),
                    stream: if stdout {
                        crate::protocol::warm::OutputStream::Stdout
                    } else {
                        crate::protocol::warm::OutputStream::Stderr
                    },
                    bytes: buf[..n].to_vec(),
                };
                match encode_warm_relay_msg(&crate::protocol::warm::WarmRelayMsg::Output(msg))
                    .map_err(io::Error::other)
                    .and_then(|body| w.send_raw(&body))
                {
                    Ok(()) => {}
                    Err(_) => break, // client gone: stop relaying
                }
            }
            Err(_) => break,
        }
    }
}

// -- Action execution --------------------------------------------------

impl<S: Spawner, C: Clock, J: Jitter> Inner<S, C, J> {
    /// Execute pool actions. Spawn: create the process, register the record
    /// (exit detection happens in `pump`'s poll). Kill: terminate.
    fn execute(&mut self, actions: &[Action]) {
        for action in actions {
            match *action {
                Action::Spawn { id, kind, .. } => match self.spawner.spawn(kind, id) {
                    Ok(process) => {
                        let pid = process.pid();
                        self.spares.insert(
                            id,
                            SpareRecord {
                                kind,
                                pid,
                                process: Some(process),
                                ready: false,
                                control: None,
                            },
                        );
                        self.slot_view[kind.index()] = "initializing".into();
                    }
                    Err(_) => {
                        let now = self.pool.clock().now();
                        let _ = self.pool.on_failed(id, now);
                        self.slot_view[kind.index()] = "backoff".into();
                    }
                },
                Action::Kill { id } => {
                    if let Some(rec) = self.spares.get_mut(&id) {
                        if let Some(p) = rec.process.as_mut() {
                            p.kill();
                        }
                    }
                }
            }
        }
    }
}

// -- Validation ---------------------------------------------------------

/// «Что в первой версии идёт мимо прогрева»: console apps, devtools/dev-url
/// flags, store-env disagreements. All are declines BEFORE any irreversible
/// action. `console: true` maps to `Unsupported(Service)`: a console app
/// requires the exact stdio/signal contract of the cold path, which a warm
/// handoff does not provide in the first version.
fn validate<S: Spawner, C: Clock, J: Jitter>(
    launch: &Launch,
    inner: &Inner<S, C, J>,
) -> Option<DeclineReason> {
    if launch.console {
        return Some(DeclineReason::Unsupported(UnsupportedReason::Service));
    }
    for arg in &launch.args {
        if os_bytes_is(arg, "--devtools") || os_bytes_is(arg, "--dev-url") {
            return Some(DeclineReason::Unsupported(UnsupportedReason::Devtools));
        }
    }
    // Exact store identity inside the launch env (including absence):
    // disagreement with the broker's own values is a validation failure.
    for change in &launch.env {
        if os_bytes_is(&change.key, "ALEF_HOME") && change.value != inner.own_alef_home {
            return Some(DeclineReason::ValidationFailed);
        }
        if os_bytes_is(&change.key, "ALEF_APP_HOME") && change.value != inner.own_alef_app_home {
            return Some(DeclineReason::ValidationFailed);
        }
    }
    None
}

/// Exact (lossless) comparison of an `OsBytes` against a plain-ASCII literal.
fn os_bytes_is(value: &OsBytes, lit: &str) -> bool {
    match value {
        OsBytes::Unix(bytes) => bytes.as_slice() == lit.as_bytes(),
        OsBytes::Windows(units) => units.as_slice() == lit.encode_utf16().collect::<Vec<_>>(),
    }
}

// Silence unused-import lint if encode_broker_msg becomes unused in
// refactors; it is used through `ReplyTo::send` implementations elsewhere.
#[allow(dead_code)]
fn _assert_send_sync<S: Spawner + Send + Sync, C: Clock + Send + Sync, J: Jitter + Send + Sync>() {
    fn assert<T: Send + Sync>() {}
    assert::<Broker<S, C, J>>();
}
