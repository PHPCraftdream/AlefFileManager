// SPDX-License-Identifier: MIT OR Apache-2.0
//! Broker-core tests: a fake [`Spawner`] whose fake spare processes feed the
//! REAL `ipc::Server` over a real endpoint in a unique temp dir. No real
//! alef processes; no sleeps — synchronization goes through channels
//! (`recv_timeout`) and deadline-guarded joins, so every wait has a deadline.

use std::collections::{HashMap, VecDeque};
use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use super::broker::Broker;
use super::spawner::{ExitInfo, SpareId, SpareProcess, Spawner};
use crate::ipc::endpoint::Endpoint;
use crate::ipc::server::Server;
use crate::ipc::Client;
use crate::pool::{Clock, Jitter, PoolConfig, SlotKind};
use crate::protocol::auth::Token;
use crate::protocol::message::{
    BrokerMsg, ClientMsg, DeclineReason, Declined, Launch, LaunchState, OsBytes, Query, Status,
    UnsupportedReason, PROTOCOL_VERSION,
};
use crate::protocol::warm::{
    decode_warm_admin_reply, decode_warm_relay_msg, encode_spare_to_broker, encode_warm_admin_msg,
    AdminShutdown, AdminStatus, AdminStatusReply, BrokerToSpare, OutputStream, Ready,
    SlotKind as WireKind, SpareHello, SpareToBroker, WarmAdminMsg, WarmAdminReply, WarmRelayMsg,
};

const DEADLINE: Duration = Duration::from_secs(5);

// -- Fakes --------------------------------------------------------------

/// Shareable fake clock; tests advance it instead of sleeping.
#[derive(Clone, Default)]
struct FakeClock {
    t: Arc<Mutex<u64>>,
}

impl FakeClock {
    fn advance(&self, d: Duration) {
        *self.t.lock().unwrap() += d.as_millis() as u64;
    }
}

impl Clock for FakeClock {
    fn now(&self) -> Duration {
        Duration::from_millis(*self.t.lock().unwrap())
    }
}

struct FixedJitter;
impl Jitter for FixedJitter {
    fn fraction(&mut self) -> f64 {
        0.0
    }
}

/// In-memory byte pipe: blocking reads, explicit EOF.
#[derive(Clone)]
struct SharedBuf {
    inner: Arc<(Mutex<VecDeque<u8>>, Condvar, AtomicBool)>,
}

impl SharedBuf {
    fn new() -> SharedBuf {
        SharedBuf {
            inner: Arc::new((
                Mutex::new(VecDeque::new()),
                Condvar::new(),
                AtomicBool::new(false),
            )),
        }
    }
    fn push(&self, bytes: &[u8]) {
        self.inner.0.lock().unwrap().extend(bytes.iter().copied());
        self.inner.1.notify_all();
    }
    fn close(&self) {
        self.inner.2.store(true, Ordering::SeqCst);
        self.inner.1.notify_all();
    }
    fn read_pop(&self, buf: &mut [u8]) -> std::io::Result<usize> {
        let (data, cv, closed) = &*self.inner;
        let mut guard = data.lock().unwrap();
        loop {
            let n = guard.len().min(buf.len());
            if n > 0 {
                for slot in buf.iter_mut().take(n) {
                    *slot = guard.pop_front().unwrap();
                }
                return Ok(n);
            }
            if closed.load(Ordering::SeqCst) {
                return Ok(0); // EOF
            }
            // Deadline-guarded: a relay bug fails the test instead of hanging.
            let (g, _) = cv.wait_timeout(guard, DEADLINE).unwrap();
            guard = g;
        }
    }
}

struct FakeReader(SharedBuf);

impl Read for FakeReader {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        self.0.read_pop(buf)
    }
}

/// The fake spare process shared between the broker's handle and the test.
struct FakeProc {
    pid: u32,
    out: SharedBuf,
    err: SharedBuf,
    exited: AtomicBool,
    killed: AtomicBool,
}

impl FakeProc {
    fn new(pid: u32) -> FakeProc {
        FakeProc {
            pid,
            out: SharedBuf::new(),
            err: SharedBuf::new(),
            exited: AtomicBool::new(false),
            killed: AtomicBool::new(false),
        }
    }
    fn exit_with(&self, _code: i32) {
        self.out.close();
        self.err.close();
        self.exited.store(true, Ordering::SeqCst);
    }
}

/// Broker-owned view of a fake process (per-handle stream-take flags).
struct FakeProcHandle {
    p: Arc<FakeProc>,
    stdout_taken: bool,
    stderr_taken: bool,
}

impl SpareProcess for FakeProcHandle {
    fn pid(&self) -> u32 {
        self.p.pid
    }
    fn kill(&self) {
        self.p.killed.store(true, Ordering::SeqCst);
    }
    fn try_wait(&mut self) -> std::io::Result<Option<ExitInfo>> {
        if self.p.exited.load(Ordering::SeqCst) {
            Ok(Some(ExitInfo {
                code: 7,
                signal: None,
            }))
        } else {
            Ok(None)
        }
    }
    fn stdout(&mut self) -> Option<Box<dyn Read + Send>> {
        if self.stdout_taken {
            None
        } else {
            self.stdout_taken = true;
            Some(Box::new(FakeReader(self.p.out.clone())))
        }
    }
    fn stderr(&mut self) -> Option<Box<dyn Read + Send>> {
        if self.stderr_taken {
            None
        } else {
            self.stderr_taken = true;
            Some(Box::new(FakeReader(self.p.err.clone())))
        }
    }
}

/// Shareable fake spawner: records created processes for the test to drive.
#[derive(Clone)]
struct FakeSpawner {
    inner: Arc<FakeSpawnerInner>,
}

struct FakeSpawnerInner {
    tx: mpsc::Sender<SpareId>,
    procs: Mutex<HashMap<SpareId, Arc<FakeProc>>>,
    next_pid: Mutex<u32>,
}

impl FakeSpawner {
    fn new() -> (FakeSpawner, mpsc::Receiver<SpareId>) {
        let (tx, rx) = mpsc::channel();
        (
            FakeSpawner {
                inner: Arc::new(FakeSpawnerInner {
                    tx,
                    procs: Mutex::new(HashMap::new()),
                    next_pid: Mutex::new(1000),
                }),
            },
            rx,
        )
    }
    fn proc_of(&self, id: SpareId) -> Arc<FakeProc> {
        self.inner.procs.lock().unwrap()[&id].clone()
    }
}

impl Spawner for FakeSpawner {
    fn spawn(&self, _kind: SlotKind, id: SpareId) -> std::io::Result<Box<dyn SpareProcess>> {
        let pid = {
            let mut n = self.inner.next_pid.lock().unwrap();
            *n += 1;
            *n
        };
        let p = Arc::new(FakeProc::new(pid));
        self.inner.procs.lock().unwrap().insert(id, Arc::clone(&p));
        self.inner.tx.send(id).unwrap();
        Ok(Box::new(FakeProcHandle {
            p,
            stdout_taken: false,
            stderr_taken: false,
        }))
    }
}

// -- Harness ------------------------------------------------------------

struct Harness {
    _tmp: tempfile::TempDir,
    ep: Endpoint,
    broker: Arc<Broker<FakeSpawner, FakeClock, FixedJitter>>,
    spawner: FakeSpawner,
    rx: mpsc::Receiver<SpareId>,
    clock: FakeClock,
    server: Arc<Server>,
    loop_handle: std::thread::JoinHandle<()>,
}

impl Harness {
    fn rx_recv_one(&self) -> SpareId {
        self.rx.recv_timeout(DEADLINE).expect("spawn notification")
    }
    fn rx_try_recv(&self) -> Option<SpareId> {
        self.rx.try_recv().ok()
    }
    fn rx_recv_filtered(&self, f: impl Fn(&SpareId) -> bool) -> SpareId {
        loop {
            let id = self.rx_recv_one();
            if f(&id) {
                return id;
            }
        }
    }
    fn stop(self) {
        self.server.stop();
        let _ = self.loop_handle.join();
        self.server.join();
    }
}

fn token() -> Arc<Token> {
    Arc::new(Token::new(vec![9u8; 32]).unwrap())
}

fn pool_cfg() -> PoolConfig {
    PoolConfig {
        enabled: true,
        windowed: true,
        windowless: true,
        ..PoolConfig::default()
    }
}

fn harness(name: &str, cfg: PoolConfig) -> (FakeSpawner, Harness) {
    let tmp = tempfile::tempdir().unwrap();
    let ep = Endpoint::for_session(tmp.path(), name);
    let (spawner, rx) = FakeSpawner::new();
    let clock = FakeClock::default();
    let broker = Arc::new(Broker::new(
        cfg,
        spawner.clone(),
        clock.clone(),
        FixedJitter,
        "test-version".into(),
    ));
    let server = Arc::new(Server::bind(&ep).expect("bind"));
    let s2 = Arc::clone(&server);
    let b2 = Arc::clone(&broker);
    let tok = token();
    let loop_handle = std::thread::spawn(move || s2.run(tok, None, b2));
    (
        spawner.clone(),
        Harness {
            _tmp: tmp,
            ep,
            broker,
            spawner,
            rx,
            clock,
            server,
            loop_handle,
        },
    )
}

fn hello() -> crate::protocol::message::Hello {
    crate::protocol::message::Hello {
        protocol_version: PROTOCOL_VERSION,
        build: "test-build".into(),
        alef_home: None,
        alef_app_home: None,
    }
}

fn connect(ep: &Endpoint) -> Client {
    Client::connect_with_hello(ep, &token(), hello()).expect("client connect")
}

/// Fake spare: the usual handshake, then SpareToBroker::Hello → Ack.
fn connect_spare(ep: &Endpoint, kind: WireKind, id: SpareId, pid: u32) -> Client {
    let mut c = connect(ep);
    let hello = SpareHello {
        kind,
        pid,
        spare_id: id.0,
    };
    c.send_raw(&encode_spare_to_broker(&SpareToBroker::Hello(hello)).unwrap())
        .unwrap();
    match recv_spare(&mut c) {
        BrokerToSpare::Ack(_) => {}
        other => panic!("expected Ack, got {other:?}"),
    }
    c
}

fn spare_ready(spare: &mut Client) {
    spare
        .send_raw(&encode_spare_to_broker(&SpareToBroker::Ready(Ready)).unwrap())
        .unwrap();
}

fn launch(rid: &str) -> Launch {
    Launch {
        request_id: rid.into(),
        target: OsBytes::from_os_string(std::ffi::OsStr::new("/apps/demo")),
        args: vec![],
        cwd: None,
        env: vec![],
        console: false,
    }
}

fn recv_broker(c: &mut Client) -> BrokerMsg {
    c.recv().expect("broker reply")
}

fn recv_relay(c: &mut Client) -> WarmRelayMsg {
    let body = c.recv_raw().expect("relay frame");
    decode_warm_relay_msg(&body).expect("relay decode")
}

fn recv_spare(c: &mut Client) -> BrokerToSpare {
    let body = c.recv_raw().expect("spare frame");
    crate::protocol::warm::decode_broker_to_spare(&body).expect("spare decode")
}

fn admin_call(c: &mut Client, msg: WarmAdminMsg) -> WarmAdminReply {
    c.send_raw(&encode_warm_admin_msg(&msg).unwrap()).unwrap();
    let body = c.recv_raw().expect("admin reply");
    decode_warm_admin_reply(&body).expect("admin decode")
}

/// Wait until the spawner produced `n` processes; returns their ids (in
/// spawn order: Windowed first, then Windowless).
fn wait_ids(rx: &mpsc::Receiver<SpareId>, n: usize) -> Vec<SpareId> {
    let deadline = Instant::now() + DEADLINE;
    let mut ids = Vec::new();
    while ids.len() < n {
        let left = deadline.saturating_duration_since(Instant::now());
        assert!(!left.is_zero(), "timeout waiting for {n} spawns");
        match rx.recv_timeout(left) {
            Ok(id) => ids.push(id),
            Err(e) => panic!("spawn channel: {e}"),
        }
    }
    ids
}

// -- Tests ---------------------------------------------------------------

#[test]
fn grant_and_same_type_replacement_in_one_take() {
    let (spawner, h) = harness("grant-replace", pool_cfg());
    h.broker.pump();
    let ids = wait_ids(&h.rx, 2); // windowed + windowless initial fill
    let win = ids[0]; // SlotKind::ALL order: Windowed first

    let mut spare = connect_spare(&h.ep, WireKind::Windowed, win, spawner.proc_of(win).pid);
    spare_ready(&mut spare);

    let mut client = connect(&h.ep);
    client.send(&ClientMsg::Launch(launch("r1"))).unwrap();
    assert!(matches!(recv_broker(&mut client), BrokerMsg::Accepted(_)));
    // Exactly one replacement spawn was scheduled in the same take.
    let replacement = h.rx_recv_one();
    assert_ne!(replacement, win);
    // The spare received the Assign carrying the launch.
    match recv_spare(&mut spare) {
        BrokerToSpare::Assign(a) => assert_eq!(a.launch.request_id, "r1"),
        other => panic!("expected Assign, got {other:?}"),
    }
    // Idempotent duplicate launch replays Accepted, no second spare taken.
    client.send(&ClientMsg::Launch(launch("r1"))).unwrap();
    assert!(matches!(recv_broker(&mut client), BrokerMsg::Accepted(_)));
    assert!(h.rx_try_recv().is_none(), "no extra spawn on duplicate");
    h.stop();
}

#[test]
fn declined_unsupported_traits() {
    let (_spawner, h) = harness("unsupported", pool_cfg());
    let mut client = connect(&h.ep);

    let mut console = launch("c1");
    console.console = true;
    client.send(&ClientMsg::Launch(console)).unwrap();
    assert!(matches!(
        recv_broker(&mut client),
        BrokerMsg::Declined(Declined {
            reason: DeclineReason::Unsupported(UnsupportedReason::Service),
            ..
        })
    ));

    let mut devtools = launch("c2");
    devtools.args = vec![OsBytes::from_os_string(std::ffi::OsStr::new("--devtools"))];
    client.send(&ClientMsg::Launch(devtools)).unwrap();
    assert!(matches!(
        recv_broker(&mut client),
        BrokerMsg::Declined(Declined {
            reason: DeclineReason::Unsupported(UnsupportedReason::Devtools),
            ..
        })
    ));

    let mut devurl = launch("c3");
    devurl.args = vec![OsBytes::from_os_string(std::ffi::OsStr::new("--dev-url"))];
    client.send(&ClientMsg::Launch(devurl)).unwrap();
    assert!(matches!(
        recv_broker(&mut client),
        BrokerMsg::Declined(Declined {
            reason: DeclineReason::Unsupported(UnsupportedReason::Devtools),
            ..
        })
    ));

    // Nothing was ever spawned for declined launches.
    assert!(h.rx_try_recv().is_none());
    h.stop();
}

#[test]
fn no_spare_when_pool_disabled() {
    let (_spawner, h) = harness(
        "no-spare",
        PoolConfig {
            enabled: false,
            ..pool_cfg()
        },
    );
    h.broker.pump();
    let mut client = connect(&h.ep);
    client.send(&ClientMsg::Launch(launch("r1"))).unwrap();
    assert!(matches!(
        recv_broker(&mut client),
        BrokerMsg::Declined(Declined {
            reason: DeclineReason::NoSpare,
            ..
        })
    ));
    h.stop();
}

#[test]
fn spare_dies_before_grant_then_slot_recovers() {
    let (spawner, h) = harness("die-before-grant", pool_cfg());
    h.broker.pump();
    let ids = wait_ids(&h.rx, 2);
    let win_id = ids[0];
    spawner.proc_of(win_id).exit_with(1); // crash before connecting
    h.broker.pump(); // notices the exit → pool.on_exited → backoff

    let mut client = connect(&h.ep);
    client.send(&ClientMsg::Launch(launch("r1"))).unwrap();
    assert!(matches!(
        recv_broker(&mut client),
        BrokerMsg::Declined(Declined {
            reason: DeclineReason::NoSpare,
            ..
        })
    ));

    // Advance the fake clock past the backoff; pump refills the slot.
    h.clock.advance(Duration::from_secs(30));
    h.broker.pump();
    let replacement = h.rx_recv_filtered(|id| *id != win_id);
    let mut spare = connect_spare(
        &h.ep,
        WireKind::Windowed,
        replacement,
        spawner.proc_of(replacement).pid,
    );
    spare_ready(&mut spare);
    client.send(&ClientMsg::Launch(launch("r2"))).unwrap();
    assert!(matches!(recv_broker(&mut client), BrokerMsg::Accepted(_)));
    h.stop();
}

#[test]
fn output_relay_and_exited() {
    let (spawner, h) = harness("relay", pool_cfg());
    h.broker.pump();
    let ids = wait_ids(&h.rx, 2);
    let win = ids[0];
    let mut spare = connect_spare(&h.ep, WireKind::Windowed, win, spawner.proc_of(win).pid);
    spare_ready(&mut spare);

    let mut client = connect(&h.ep);
    client.send(&ClientMsg::Launch(launch("r1"))).unwrap();
    assert!(matches!(recv_broker(&mut client), BrokerMsg::Accepted(_)));
    let _ = recv_spare(&mut spare); // Assign

    let proc = spawner.proc_of(win);
    proc.out.push(b"hello ");
    proc.out.push(b"world");
    proc.err.push(b"oops");
    proc.exit_with(7);

    let mut stdout: Vec<u8> = Vec::new();
    let mut stderr: Vec<u8> = Vec::new();
    let mut exited = false;
    let deadline = Instant::now() + DEADLINE;
    while !exited {
        assert!(Instant::now() < deadline, "relay timeout");
        match recv_relay(&mut client) {
            WarmRelayMsg::Output(o) => {
                assert_eq!(o.request_id, "r1");
                assert!(o.bytes.len() <= 32 * 1024);
                if o.stream == OutputStream::Stdout {
                    stdout.extend(o.bytes);
                } else {
                    stderr.extend(o.bytes);
                }
            }
            WarmRelayMsg::Exited(e) => {
                assert_eq!(e.code, 7);
                assert_eq!(e.signal, None);
                exited = true;
            }
        }
    }
    assert_eq!(stdout, b"hello world");
    assert_eq!(stderr, b"oops");

    // Bookkeeping settled by the next pump; Query from a NEW connection
    // resolves the finished launch.
    // The finisher of the relay settles the log right after the Exited frame left: ask again
    // (a real wait, no fixed pause) until the log says so.
    let mut probe = connect(&h.ep);
    let mut settled = false;
    for _ in 0..200 {
        h.broker.pump();
        probe
            .send(&ClientMsg::Query(Query {
                request_id: "r1".into(),
            }))
            .unwrap();
        if matches!(
            recv_broker(&mut probe),
            BrokerMsg::Status(Status {
                state: LaunchState::HandedOff,
                ..
            })
        ) {
            settled = true;
            break;
        }
        std::thread::sleep(std::time::Duration::from_millis(25));
    }
    assert!(settled, "the log never settled to HandedOff");
    h.stop();
}

#[test]
fn shutdown_keeps_granted_app_untouched() {
    let (spawner, h) = harness("shutdown", pool_cfg());
    h.broker.pump();
    let ids = wait_ids(&h.rx, 2);
    let win = ids[0];
    let other = ids[1];
    let mut spare = connect_spare(&h.ep, WireKind::Windowed, win, spawner.proc_of(win).pid);
    spare_ready(&mut spare);

    let mut client = connect(&h.ep);
    client.send(&ClientMsg::Launch(launch("r1"))).unwrap();
    assert!(matches!(recv_broker(&mut client), BrokerMsg::Accepted(_)));
    let _ = recv_spare(&mut spare); // Assign

    h.broker.shutdown();
    // The granted process was NOT touched; the pool-owned one was killed.
    assert!(!spawner.proc_of(win).killed.load(Ordering::SeqCst));
    assert!(spawner.proc_of(other).killed.load(Ordering::SeqCst));

    // The granted app keeps relaying output after shutdown.
    let proc = spawner.proc_of(win);
    proc.out.push(b"still alive");
    proc.out.close();
    proc.exit_with(0);
    let mut got = Vec::new();
    let deadline = Instant::now() + DEADLINE;
    loop {
        assert!(Instant::now() < deadline, "post-shutdown relay timeout");
        match recv_relay(&mut client) {
            WarmRelayMsg::Output(o) => got.extend(o.bytes),
            WarmRelayMsg::Exited(_) => break,
        }
    }
    assert_eq!(got, b"still alive");

    // New launches are declined while shutting down.
    let mut late = connect(&h.ep);
    late.send(&ClientMsg::Launch(launch("r2"))).unwrap();
    assert!(matches!(
        recv_broker(&mut late),
        BrokerMsg::Declined(Declined {
            reason: DeclineReason::Busy,
            ..
        })
    ));
    h.stop();
}

#[test]
fn query_after_client_disconnect() {
    let (spawner, h) = harness("query-disconnect", pool_cfg());
    h.broker.pump();
    let ids = wait_ids(&h.rx, 2);
    let win = ids[0];
    let mut spare = connect_spare(&h.ep, WireKind::Windowed, win, spawner.proc_of(win).pid);
    spare_ready(&mut spare);

    let mut client = connect(&h.ep);
    client.send(&ClientMsg::Launch(launch("r1"))).unwrap();
    assert!(matches!(recv_broker(&mut client), BrokerMsg::Accepted(_)));
    let _ = recv_spare(&mut spare);
    drop(client); // ambiguous ack → query from a new connection

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

#[test]
fn admin_status_fields() {
    let (spawner, h) = harness("admin", pool_cfg());
    h.broker.pump();
    let ids = wait_ids(&h.rx, 2);
    let win = ids[0];
    let mut spare = connect_spare(&h.ep, WireKind::Windowed, win, spawner.proc_of(win).pid);
    spare_ready(&mut spare);

    let mut admin = connect(&h.ep);
    match admin_call(&mut admin, WarmAdminMsg::Status(AdminStatus)) {
        WarmAdminReply::StatusReply(s) => {
            let AdminStatusReply {
                pid,
                version,
                slots,
                granted_total,
                running,
            } = s;
            assert_eq!(pid, std::process::id());
            assert_eq!(version, "test-version");
            assert_eq!(slots.len(), 2);
            let windowed = slots
                .iter()
                .find(|e| e.kind == WireKind::Windowed)
                .expect("windowed slot");
            assert_eq!(windowed.state, "ready");
            assert_eq!(granted_total, 0);
            assert_eq!(running, 2);
        }
        other => panic!("expected StatusReply, got {other:?}"),
    }
    h.stop();
}

#[test]
fn admin_shutdown_acks_and_stops() {
    let (_spawner, h) = harness("admin-stop", pool_cfg());
    h.broker.pump();
    let _ids = wait_ids(&h.rx, 2);
    let mut admin = connect(&h.ep);
    match admin_call(&mut admin, WarmAdminMsg::Shutdown(AdminShutdown)) {
        WarmAdminReply::Ack(_) => {}
        other => panic!("expected Ack, got {other:?}"),
    }
    let mut late = connect(&h.ep);
    late.send(&ClientMsg::Launch(launch("r1"))).unwrap();
    assert!(matches!(
        recv_broker(&mut late),
        BrokerMsg::Declined(Declined {
            reason: DeclineReason::Busy,
            ..
        })
    ));
    h.stop();
}
