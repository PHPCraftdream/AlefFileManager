// SPDX-License-Identifier: MIT OR Apache-2.0
//! Peer-library tests against a REAL [`Broker`] with a FAKE spawner: unique
//! temp dir, `Endpoint::for_session`, real `ipc::Server` with the token from
//! `load_or_create_token` (the token file is what peers actually read). No
//! real processes, no sleeps — channels with `recv_timeout` and deadline
//! guards only.

use std::collections::{HashMap, VecDeque};
use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Condvar, Mutex};
use std::time::{Duration, Instant};

use super::client::{try_launch, ColdReason, Outcome};
use super::spare::{Assignment, Spare};
use crate::broker::{Broker, ExitInfo, SpareId, SpareProcess, Spawner};
use crate::ipc::endpoint::Endpoint;
use crate::ipc::server::Server;
use crate::ipc::token::load_or_create_token;
use crate::pool::{Clock, Jitter, PoolConfig, SlotKind};
use crate::protocol::message::OsBytes;
use crate::protocol::warm::SlotKind as WireKind;

const DEADLINE: Duration = Duration::from_secs(5);

/// Busy-wait until a broker-side condition holds (deadline-guarded, no sleep).
fn wait_until(f: impl Fn() -> bool) {
    let deadline = Instant::now() + DEADLINE;
    while !f() {
        assert!(Instant::now() < deadline, "condition timeout");
        std::thread::yield_now();
    }
}

// -- Fakes (same patterns as broker/tests.rs) ----------------------------

#[derive(Clone, Default)]
struct FakeClock {
    t: Arc<Mutex<u64>>,
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

/// In-memory byte pipe: blocking reads, explicit EOF, deadline-guarded wait.
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

struct FakeProc {
    pid: u32,
    out: SharedBuf,
    err: SharedBuf,
    exit_code: Mutex<Option<i32>>,
}

impl FakeProc {
    fn new(pid: u32) -> FakeProc {
        FakeProc {
            pid,
            out: SharedBuf::new(),
            err: SharedBuf::new(),
            exit_code: Mutex::new(None),
        }
    }
    fn exit_with(&self, code: i32) {
        *self.exit_code.lock().unwrap() = Some(code);
        self.out.close();
        self.err.close();
    }
}

struct FakeProcHandle {
    p: Arc<FakeProc>,
    stdout_taken: bool,
    stderr_taken: bool,
}

impl SpareProcess for FakeProcHandle {
    fn pid(&self) -> u32 {
        self.p.pid
    }
    fn kill(&self) {}
    fn try_wait(&mut self) -> std::io::Result<Option<ExitInfo>> {
        if let Some(code) = self.p.exit_code.lock().unwrap().take() {
            Ok(Some(ExitInfo { code, signal: None }))
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

// -- Harness --------------------------------------------------------------

struct Harness {
    _tmp: tempfile::TempDir,
    ep: Endpoint,
    broker: Arc<Broker<FakeSpawner, FakeClock, FixedJitter>>,
    spawner: FakeSpawner,
    rx: mpsc::Receiver<SpareId>,
    server: Arc<Server>,
    loop_handle: std::thread::JoinHandle<()>,
}

impl Harness {
    fn wait_ids(&self, n: usize) -> Vec<SpareId> {
        let deadline = Instant::now() + DEADLINE;
        let mut ids = Vec::new();
        while ids.len() < n {
            let left = deadline.saturating_duration_since(Instant::now());
            assert!(!left.is_zero(), "timeout waiting for {n} spawns");
            match self.rx.recv_timeout(left) {
                Ok(id) => ids.push(id),
                Err(e) => panic!("spawn channel: {e}"),
            }
        }
        ids
    }
    fn stop(self) {
        self.server.stop();
        let _ = self.loop_handle.join();
        self.server.join();
    }
}

fn pool_cfg() -> PoolConfig {
    PoolConfig {
        enabled: true,
        windowed: true,
        windowless: true,
        ..PoolConfig::default()
    }
}

/// Broker + real `ipc::Server` on a unique temp endpoint, using the token
/// from the token file (that is the file peers load).
fn harness(name: &str, cfg: PoolConfig) -> Harness {
    let tmp = tempfile::tempdir().unwrap();
    let ep = Endpoint::for_session(tmp.path(), name);
    let (spawner, rx) = FakeSpawner::new();
    let broker = Arc::new(Broker::new(
        cfg,
        spawner.clone(),
        FakeClock::default(),
        FixedJitter,
        "test-version".into(),
    ));
    let server = Arc::new(Server::bind(&ep).expect("bind"));
    let s2 = Arc::clone(&server);
    let b2: Arc<dyn crate::ipc::Handler> = Arc::clone(&broker) as _;
    // The file token: peers (Spare, try_launch) load exactly this.
    let (token, created) = load_or_create_token(&ep).expect("token file");
    assert!(created);
    let loop_handle = std::thread::spawn(move || s2.run(token.into(), None, b2));
    Harness {
        _tmp: tmp,
        ep,
        broker,
        spawner,
        rx,
        server,
        loop_handle,
    }
}

fn launch(rid: &str) -> crate::protocol::message::Launch {
    crate::protocol::message::Launch {
        request_id: rid.into(),
        target: OsBytes::from_os_string(std::ffi::OsStr::new("/apps/demo")),
        args: vec![],
        cwd: None,
        env: vec![],
        console: false,
    }
}

// -- Tests ----------------------------------------------------------------

/// The real spare path, end to end: `Spare::connect` → `ready` → `Assign`,
/// then the fake process produces output and exits — `try_launch` reports
/// `Handled` with the exit code and the relayed bytes land in the Writes.
#[test]
fn try_launch_handled_with_relayed_output_and_exit_code() {
    let h = harness("peer-handled", pool_cfg());
    h.broker.pump();
    let ids = h.wait_ids(2);
    let win = ids[0];
    let proc = h.spawner.proc_of(win);

    // The spare runs on its own thread: connect, ready, take the assignment,
    // emit output and exit — all while the main thread is inside try_launch.
    let ep = h.ep.clone();
    let proc2 = Arc::clone(&proc);
    let (ready_tx, ready_rx) = mpsc::channel();
    let spare_thread = std::thread::spawn(move || {
        let mut spare = Spare::connect(&ep, WireKind::Windowed, win.0).expect("spare connect");
        assert_eq!(spare.kind, WireKind::Windowed);
        spare.ready().expect("ready");
        ready_tx.send(()).expect("ready signal");
        match spare.wait_assignment(DEADLINE) {
            Ok(Assignment::Launch(l)) => assert_eq!(l.request_id, "r1"),
            other => panic!("expected assignment, got {other:?}"),
        }
        proc2.out.push(b"hello out");
        proc2.err.push(b"err!");
        proc2.exit_with(7);
    });

    // Wait until the broker has actually processed the spare's Ready.
    ready_rx
        .recv_timeout(DEADLINE)
        .expect("spare readiness signal");
    wait_until(|| {
        h.broker
            .admin_status()
            .slots
            .iter()
            .any(|s| s.kind == WireKind::Windowed && s.state == "ready")
    });

    let mut stdout: Vec<u8> = Vec::new();
    let mut stderr: Vec<u8> = Vec::new();
    let outcome = try_launch(&h.ep, &launch("r1"), &mut stdout, &mut stderr);
    spare_thread.join().expect("spare thread");
    assert_eq!(outcome, Outcome::Handled { exit_code: 7 });
    assert_eq!(stdout, b"hello out");
    assert_eq!(stderr, b"err!");
    h.stop();
}

/// No broker at the entry point: a PROVEN cold reason.
#[test]
fn try_launch_without_broker_is_cold_no_broker() {
    let tmp = tempfile::tempdir().unwrap();
    let ep = Endpoint::for_session(tmp.path(), "peer-no-broker");
    let mut stdout: Vec<u8> = Vec::new();
    let mut stderr: Vec<u8> = Vec::new();
    let outcome = try_launch(&ep, &launch("r1"), &mut stdout, &mut stderr);
    assert_eq!(outcome, Outcome::Cold(ColdReason::NoBroker));
    assert!(stdout.is_empty() && stderr.is_empty());
}

/// A declined request (`--devtools`) reaches the caller as a PROVEN
/// `Cold(Declined(..))`, before any spare is taken.
#[test]
fn try_launch_declined_request_is_cold_declined() {
    let h = harness("peer-declined", pool_cfg());
    let mut req = launch("r1");
    req.args = vec![OsBytes::from_os_string(std::ffi::OsStr::new("--devtools"))];
    let mut stdout: Vec<u8> = Vec::new();
    let mut stderr: Vec<u8> = Vec::new();
    let outcome = try_launch(&h.ep, &req, &mut stdout, &mut stderr);
    assert!(matches!(
        outcome,
        Outcome::Cold(ColdReason::Declined(
            crate::protocol::message::DeclineReason::Unsupported(
                crate::protocol::message::UnsupportedReason::Devtools
            )
        ))
    ));
    h.stop();
}

/// Spare-side happy path against the real broker: `connect` → `ready`, the
/// launch delivers exactly one `Assign` carrying the launch.
#[test]
fn spare_connect_ready_receives_assign() {
    let h = harness("peer-spare-assign", pool_cfg());
    h.broker.pump();
    let ids = h.wait_ids(2);
    let win = ids[0];

    let mut spare = Spare::connect(&h.ep, WireKind::Windowed, win.0).expect("spare connect");
    spare.ready().expect("ready");

    // Drive the broker from a plain client connection (raw Launch).
    let mut client = crate::ipc::Client::connect(&h.ep, &{
        let (t, _) = load_or_create_token(&h.ep).unwrap();
        t
    })
    .expect("client connect");
    client
        .send(&crate::protocol::message::ClientMsg::Launch(launch("r1")))
        .unwrap();
    let deadline = Instant::now() + DEADLINE;
    loop {
        assert!(Instant::now() < deadline, "no ack");
        match client.recv().expect("broker reply") {
            crate::protocol::message::BrokerMsg::Accepted(_) => break,
            crate::protocol::message::BrokerMsg::HelloAck(_) => continue,
            other => panic!("expected Accepted, got {other:?}"),
        }
    }
    match spare.wait_assignment(DEADLINE) {
        Ok(Assignment::Launch(l)) => assert_eq!(l.request_id, "r1"),
        other => panic!("expected Assign, got {other:?}"),
    }
    h.stop();
}

/// Spare-side decline path: a spare that refuses (instead of `ready`) is
/// retired by the broker, and the next launch ends in a client-visible
/// `Declined` (no spare left to grant).
#[test]
fn spare_decline_leads_to_client_declined() {
    let h = harness("peer-spare-decline", pool_cfg());
    h.broker.pump();
    let ids = h.wait_ids(2);
    let win = ids[0];

    let mut spare = Spare::connect(&h.ep, WireKind::Windowed, win.0).expect("spare connect");
    spare.decline("warm-up failed").expect("decline");
    // Wait until the broker retired the spare (slot backoff, process gone).
    wait_until(|| h.broker.admin_status().running == 1);

    // The broker retired the spare on the spot (no pump needed); the slot is
    // in backoff, so the launch is declined with NoSpare.
    let mut client = crate::ipc::Client::connect(&h.ep, &{
        let (t, _) = load_or_create_token(&h.ep).unwrap();
        t
    })
    .expect("client connect");
    client
        .send(&crate::protocol::message::ClientMsg::Launch(launch("r1")))
        .unwrap();
    let deadline = Instant::now() + DEADLINE;
    let state = loop {
        assert!(Instant::now() < deadline, "no broker reply");
        match client.recv().expect("broker reply") {
            crate::protocol::message::BrokerMsg::Declined(d) => break d.reason,
            crate::protocol::message::BrokerMsg::HelloAck(_) => continue,
            other => panic!("expected Declined, got {other:?}"),
        }
    };
    assert!(matches!(
        state,
        crate::protocol::message::DeclineReason::NoSpare
    ));
    // Admin snapshot agrees: only the untouched windowless spare remains.
    assert_eq!(h.broker.admin_status().running, 1);
    h.stop();
}
