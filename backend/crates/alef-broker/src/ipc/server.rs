// SPDX-License-Identifier: MIT OR Apache-2.0
//! Broker accept loop and per-connection lifecycle
//! (docs/stages/m11-warm-start.md, «Принятие запроса и жизненный цикл broker»).
//!
//! Per connection: OS peer check (uid / SID+session), mutual MAC handshake
//! (`protocol::auth`, fresh `RandomNonce` nonces), a mandatory `Hello`
//! validated against the expected identity, then a framed message loop with
//! `FrameDecoder` + `Limits` (TokenBucket over real milliseconds), idle
//! timeouts and fail-closed closes. Actual message handling is delegated to
//! [`Handler`]; the transport owns no launch logic.

use std::io::{self, ErrorKind};

use crate::protocol::message::BrokerMsg;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::ipc::endpoint::Endpoint;
use crate::ipc::{
    write_frame, Conn, ConnWriter, FrameReader, Listener, PeerIdentity, ReplyTo,
    HANDSHAKE_CLIENT_REPLY_LEN,
};
use crate::protocol::auth::Token;
use crate::protocol::auth::{server_finish_mac, server_verify_client, NonceSource, RandomNonce};
use crate::protocol::frame::{Limits, TokenBucket};
use crate::protocol::message::{
    decode_client_msg, ClientMsg, Hello, HelloAck, RejectReason, PROTOCOL_VERSION,
};

/// Hard cap on simultaneously served connections per server.
pub const MAX_CONNECTIONS: u32 = 16;
/// Handshake read/write timeout.
pub const HANDSHAKE_TIMEOUT: Duration = Duration::from_secs(5);
/// Idle timeout between frames on an established connection.
pub const IDLE_TIMEOUT: Duration = Duration::from_secs(30);

/// Time slice of one blocking read on a SHARED connection: detached writers
/// (spare `Output`/`Exited` relay) need the connection lock between reads,
/// so the loop releases it at least this often. Total idleness is still
/// bounded by [`IDLE_TIMEOUT`], tracked manually across slices.
pub const SHARED_READ_SLICE: Duration = Duration::from_millis(50);

/// What a broker does with an authenticated, compatible client message.
pub trait Handler: Send + Sync {
    /// Answer via `conn.send(..)`; the transport closes the connection when
    /// this returns, so handlers must not block indefinitely.
    fn on_message(&self, conn: &mut dyn ReplyTo, msg: ClientMsg);

    /// A post-handshake frame body that is NOT a valid `ClientMsg` — used by
    /// spare (`SpareToBroker`) and admin (`WarmAdminMsg`) connections, which
    /// share the same handshake but speak extra frame kinds. `conn_id`
    /// identifies the connection for the broker's bookkeeping. Returning
    /// `Err` closes the connection with `BadRequest` (default behaviour).
    fn on_raw_frame(&self, conn: &mut dyn ReplyTo, conn_id: u64, body: &[u8]) -> io::Result<()> {
        let _ = (conn, conn_id, body);
        Err(io::Error::new(
            ErrorKind::InvalidData,
            "frame is not a client message",
        ))
    }
}

/// Identity the broker expects from the client's `Hello`. `None` on `bind`
/// disables deep validation (the MAC handshake still runs).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ExpectedIdentity {
    pub build: String,
    pub alef_home: Option<crate::protocol::message::OsBytes>,
    pub alef_app_home: Option<crate::protocol::message::OsBytes>,
}

struct ServerInner {
    stopping: AtomicBool,
    listener: Box<dyn Listener>,
    active: AtomicUsize,
    threads: Mutex<Vec<JoinHandle<()>>>,
}

/// A bound broker server. `run` blocks on the accept loop; `stop` + `join`
/// shut it down from another thread.
pub struct Server {
    inner: Arc<ServerInner>,
}

impl Server {
    /// Bind the platform endpoint for `ep`. Unix: private dir + 0600 socket.
    /// Windows: FIRST_INSTANCE pipe with the current user's DACL.
    pub fn bind(ep: &Endpoint) -> io::Result<Server> {
        #[cfg(unix)]
        let listener: Box<dyn Listener> =
            Box::new(crate::ipc::unix::UnixEndpointListener::bind(ep)?);
        #[cfg(windows)]
        let listener: Box<dyn Listener> = Box::new(crate::ipc::windows::PipeListener::bind(
            ep,
            &crate::ipc::windows::current_user_sid()?,
        )?);
        Ok(Server {
            inner: Arc::new(ServerInner {
                stopping: AtomicBool::new(false),
                listener,
                active: AtomicUsize::new(0),
                threads: Mutex::new(Vec::new()),
            }),
        })
    }

    /// Run the accept loop until [`Server::stop`]. Token is shared with the
    /// per-connection threads via `Arc` (the secret is never copied).
    pub fn run(
        &self,
        token: Arc<Token>,
        expected: Option<ExpectedIdentity>,
        handler: Arc<dyn Handler>,
    ) {
        while !self.inner.stopping.load(Ordering::SeqCst) {
            match self.inner.listener.accept() {
                Ok(conn) => {
                    if self.inner.stopping.load(Ordering::SeqCst) {
                        continue; // wake-dummy or race with stop
                    }
                    let over_budget =
                        self.inner.active.load(Ordering::SeqCst) >= MAX_CONNECTIONS as usize;
                    let inner = Arc::clone(&self.inner);
                    let token = Arc::clone(&token);
                    let expected = expected.clone();
                    let handler = Arc::clone(&handler);
                    if let Ok(handle) =
                        std::thread::Builder::new()
                            .name("ipc-conn".into())
                            .spawn(move || {
                                inner.active.fetch_add(1, Ordering::SeqCst);
                                let res = serve_conn(
                                    conn,
                                    &token,
                                    expected.as_ref(),
                                    &*handler,
                                    &inner.stopping,
                                    over_budget,
                                );
                                // Errors just close the connection: nothing sent
                                // before the ack can be misread as acceptance.
                                drop(res);
                                inner.active.fetch_sub(1, Ordering::SeqCst);
                            })
                    {
                        self.inner
                            .threads
                            .lock()
                            .expect("threads mutex")
                            .push(handle);
                    }
                }
                Err(e) => {
                    // wake-dummy disconnects, accept gets a transient error.
                    if matches!(
                        e.kind(),
                        ErrorKind::ConnectionAborted
                            | ErrorKind::ConnectionReset
                            | ErrorKind::Interrupted
                            | ErrorKind::WouldBlock
                    ) {
                        continue;
                    }
                    if self.inner.stopping.load(Ordering::SeqCst) {
                        break;
                    }
                    // Unknown accept error: fail the loop, but never the
                    // already-accepted connections (they run on their threads).
                    break;
                }
            }
        }
    }

    /// Signal the accept loop to stop and wake the pending accept.
    pub fn stop(&self) {
        self.inner.stopping.store(true, Ordering::SeqCst);
        self.inner.listener.wake();
    }

    /// Join all per-connection threads (call after the accept loop exited).
    pub fn join(&self) {
        let mut threads = self.inner.threads.lock().expect("threads mutex");
        for h in threads.drain(..) {
            let _ = h.join();
        }
    }
}

/// A connection shared between the serve loop and detached writers via
/// `Arc<Mutex<..>>`: all I/O (reads and writes) goes through the one lock, so
/// frames never interleave. Poisoned locks are tolerated (the connection is
/// torn down anyway).
pub struct SharedReply {
    conn: Arc<Mutex<Box<dyn Conn>>>,
}

impl SharedReply {
    pub fn new(conn: Box<dyn Conn>) -> SharedReply {
        SharedReply {
            conn: Arc::new(Mutex::new(conn)),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Box<dyn Conn>> {
        self.conn.lock().unwrap_or_else(|p| p.into_inner())
    }
}

impl ReplyTo for SharedReply {
    fn send(&mut self, msg: &BrokerMsg) -> io::Result<()> {
        ReplyTo::send(&mut *self.lock(), msg)
    }

    fn send_raw(&mut self, body: &[u8]) -> io::Result<()> {
        write_frame(&mut *self.lock(), body)
    }

    fn detach(&mut self) -> io::Result<Box<dyn ConnWriter + Send>> {
        Ok(Box::new(SharedReply {
            conn: Arc::clone(&self.conn),
        }))
    }
}

impl ConnWriter for SharedReply {
    fn send(&mut self, msg: &BrokerMsg) -> io::Result<()> {
        ReplyTo::send(self as &mut dyn ReplyTo, msg)
    }

    fn send_raw(&mut self, body: &[u8]) -> io::Result<()> {
        write_frame(&mut *self.lock(), body)
    }
}

fn serve_conn(
    mut conn: Box<dyn Conn>,
    token: &Token,
    expected: Option<&ExpectedIdentity>,
    handler: &dyn Handler,
    stopping: &AtomicBool,
    over_budget: bool,
) -> io::Result<()> {
    conn.set_read_timeout(Some(HANDSHAKE_TIMEOUT))?;
    conn.set_write_timeout(Some(HANDSHAKE_TIMEOUT))?;

    // OS-level peer check: the transport never trusts the peer's own claims.
    let own = own_identity()?;
    if conn.peer_identity()? != own {
        let _ = reject(&mut conn, RejectReason::AuthFailed);
        return Err(io::Error::new(ErrorKind::PermissionDenied, "foreign peer"));
    }
    if over_budget {
        let _ = reject(&mut conn, RejectReason::TooMany);
        return Ok(());
    }

    // 1. server -> client: nonce_s
    let nonce_s = RandomNonce.generate();
    write_frame(&mut conn, &nonce_s)?;

    // 2. client -> server: nonce_c || mac_client
    let mut frames = FrameReader::new();
    let reply = frames.read_frame(&mut conn)?;
    if reply.len() != HANDSHAKE_CLIENT_REPLY_LEN {
        let _ = reject(&mut conn, RejectReason::AuthFailed);
        return Err(io::Error::new(
            ErrorKind::InvalidData,
            "bad handshake length",
        ));
    }
    let (nonce_c_bytes, mac) = reply.split_at(crate::protocol::auth::NONCE_LEN);
    let mut nonce_c = [0u8; crate::protocol::auth::NONCE_LEN];
    nonce_c.copy_from_slice(nonce_c_bytes);
    let mut client_mac = [0u8; 32];
    client_mac.copy_from_slice(mac);
    if !server_verify_client(token, &nonce_s, &nonce_c, &client_mac) {
        let _ = reject(&mut conn, RejectReason::AuthFailed);
        return Err(io::Error::new(
            ErrorKind::PermissionDenied,
            "bad client MAC",
        ));
    }

    // 3. server -> client: mac_server
    write_frame(&mut conn, &server_finish_mac(token, &nonce_c, &nonce_s))?;

    // 4. Hello exchange: first framed message after the MAC.
    conn.set_read_timeout(Some(IDLE_TIMEOUT))?;
    let body = frames.read_frame(&mut conn)?;
    let hello = match decode_client_msg(&body) {
        Ok(ClientMsg::Hello(h)) => h,
        Ok(_) => {
            let _ = reject(&mut conn, RejectReason::BadRequest);
            return Err(io::Error::new(ErrorKind::InvalidData, "expected Hello"));
        }
        Err(_) => {
            let _ = reject(&mut conn, RejectReason::BadRequest);
            return Err(io::Error::new(ErrorKind::InvalidData, "undecodable Hello"));
        }
    };
    if let Some(exp) = expected {
        if let Some(reason) = hello_mismatch(&hello, exp) {
            let _ = reject(&mut conn, reason);
            return Err(io::Error::new(
                ErrorKind::PermissionDenied,
                "hello mismatch",
            ));
        }
    }
    let ack_build = match expected {
        Some(exp) => exp.build.clone(),
        None => hello.build.clone(),
    };
    ReplyTo::send(
        &mut conn,
        &crate::protocol::message::BrokerMsg::HelloAck(HelloAck {
            protocol_version: PROTOCOL_VERSION,
            build: ack_build,
        }),
    )?;

    // From here the connection is shared: the broker relays spare
    // Output/Exited frames from other threads, so the loop's reads and every
    // write go through the one lock in `SharedReply`. Reads are time-sliced
    // (SHARED_READ_SLICE, WouldBlock → retry) so detached writers get the
    // lock; total idleness is bounded by IDLE_TIMEOUT, tracked manually.
    conn.set_read_timeout(Some(SHARED_READ_SLICE))?;
    let mut reply = SharedReply::new(conn);

    // Message loop: limits per real elapsed milliseconds, fail-closed closes.
    let conn_id = next_conn_id();
    let limits = Limits::default();
    let mut bucket = TokenBucket::new(limits.frames_per_second, limits.frames_per_second);
    let start = Instant::now();
    let mut last_activity = Instant::now();
    loop {
        if stopping.load(Ordering::SeqCst) {
            let _ = reject(&mut reply, RejectReason::ShuttingDown);
            return Ok(());
        }
        let body = {
            let mut guard = reply.lock();
            match frames.read_frame(&mut *guard) {
                Ok(body) => {
                    last_activity = Instant::now();
                    body
                }
                // Read slice elapsed: release the lock so detached writers
                // can relay, then read again.
                Err(e) if e.kind() == ErrorKind::WouldBlock => {
                    drop(guard);
                    if last_activity.elapsed() > IDLE_TIMEOUT {
                        return Err(io::Error::new(
                            ErrorKind::TimedOut,
                            "idle timeout on shared connection",
                        ));
                    }
                    continue;
                }
                Err(e) => return Err(e),
            }
        };
        let now_ms = start.elapsed().as_millis() as u64;
        if !bucket.allow(now_ms) {
            // Rate limited: the stream is still synchronized, but fail-closed
            // keeps the budget simple and per docs rejects at the connection.
            let _ = reject(&mut reply, RejectReason::RateLimited);
            return Ok(());
        }
        match decode_client_msg(&body) {
            Ok(ClientMsg::Hello(_)) => {
                let _ = reject(&mut reply, RejectReason::IncompatibleProtocol);
                return Err(io::Error::new(ErrorKind::InvalidData, "duplicate Hello"));
            }
            Ok(msg) => handler.on_message(&mut reply, msg),
            // Not a ClientMsg: spare/admin frames go to the raw hook; its
            // error closes the connection with BadRequest (fail-closed).
            Err(_) => {
                if handler.on_raw_frame(&mut reply, conn_id, &body).is_err() {
                    let _ = reject(&mut reply, RejectReason::BadRequest);
                    return Err(io::Error::new(ErrorKind::InvalidData, "undecodable frame"));
                }
            }
        }
    }
}

/// Compare a client `Hello` against the expected identity; `None` = match.
fn hello_mismatch(hello: &Hello, expected: &ExpectedIdentity) -> Option<RejectReason> {
    if hello.protocol_version != PROTOCOL_VERSION {
        return Some(RejectReason::IncompatibleProtocol);
    }
    if hello.build != expected.build {
        return Some(RejectReason::IncompatibleBuild);
    }
    if hello.alef_home != expected.alef_home || hello.alef_app_home != expected.alef_app_home {
        return Some(RejectReason::StoreMismatch);
    }
    None
}

fn reject(conn: &mut dyn ReplyTo, reason: RejectReason) -> io::Result<()> {
    use crate::protocol::message::{BrokerMsg, Reject};
    ReplyTo::send(conn, &BrokerMsg::Reject(Reject { reason }))
}
/// Monotonic per-server connection id for `Handler::on_raw_frame`.
fn next_conn_id() -> u64 {
    use std::sync::atomic::AtomicU64;
    static NEXT: AtomicU64 = AtomicU64::new(1);
    NEXT.fetch_add(1, Ordering::SeqCst)
}

/// Own OS identity; a peer must present exactly this.
pub fn own_identity() -> io::Result<PeerIdentity> {
    #[cfg(unix)]
    {
        crate::ipc::unix::own_identity()
    }
    #[cfg(windows)]
    {
        crate::ipc::windows::own_identity()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ipc::client::Client;
    use crate::ipc::endpoint::Endpoint;
    use crate::ipc::ConnectError;
    use crate::protocol::auth::Token;
    use crate::protocol::message::{BrokerMsg, LaunchState, Query, Status};

    struct EchoHandler;
    impl Handler for EchoHandler {
        fn on_message(&self, conn: &mut dyn ReplyTo, msg: ClientMsg) {
            if let ClientMsg::Query(q) = msg {
                let _ = ReplyTo::send(
                    conn,
                    &crate::protocol::message::BrokerMsg::Status(Status {
                        request_id: q.request_id,
                        state: LaunchState::Unknown,
                    }),
                );
            }
        }
    }

    fn test_token(tag: u8) -> Arc<Token> {
        Arc::new(Token::new(vec![tag; 32]).unwrap())
    }

    /// Start a server on a fresh endpoint; returns the handle and the client
    /// readiness signal (signalled once the accept loop has started).
    fn start_server(
        ep: &Endpoint,
        token: Arc<Token>,
        expected: Option<ExpectedIdentity>,
    ) -> (Arc<Server>, std::thread::JoinHandle<()>) {
        let server = Arc::new(Server::bind(ep).expect("bind"));
        let s2 = Arc::clone(&server);
        let handle = std::thread::spawn(move || {
            s2.run(token, expected, Arc::new(EchoHandler));
        });
        (server, handle)
    }

    fn hello(build: &str) -> Hello {
        Hello {
            protocol_version: PROTOCOL_VERSION,
            build: build.into(),
            alef_home: None,
            alef_app_home: None,
        }
    }

    #[test]
    fn handshake_frames_and_stop() {
        let tmp = tempfile::tempdir().unwrap();
        let ep = Endpoint::for_session(tmp.path(), "server-tests");
        let (server, loop_handle) = start_server(
            &ep,
            test_token(1),
            Some(ExpectedIdentity {
                build: "test-build".into(),
                alef_home: None,
                alef_app_home: None,
            }),
        );

        let mut client =
            Client::connect_with_hello(&ep, &test_token(1), hello("test-build")).expect("connect");
        client
            .send(&ClientMsg::Query(Query {
                request_id: "r1".into(),
            }))
            .unwrap();
        match client.recv().expect("recv") {
            BrokerMsg::Status(s) => {
                assert_eq!(s.request_id, "r1");
                assert_eq!(s.state, LaunchState::Unknown);
            }
            other => panic!("unexpected message: {other:?}"),
        }
        // Multiple frames over one connection.
        for i in 0..5 {
            client
                .send(&ClientMsg::Query(Query {
                    request_id: format!("r{i}"),
                }))
                .unwrap();
            assert!(matches!(client.recv().unwrap(), BrokerMsg::Status(_)));
        }

        server.stop();
        loop_handle.join().unwrap();
        server.join();
    }

    #[test]
    fn wrong_token_is_auth_failed() {
        let tmp = tempfile::tempdir().unwrap();
        let ep = Endpoint::for_session(tmp.path(), "wrong-token");
        let (server, loop_handle) = start_server(&ep, test_token(2), None);
        let err = Client::connect_with_hello(&ep, &test_token(3), hello("b")).unwrap_err();
        assert!(matches!(err, ConnectError::AuthFailed), "{err}");
        server.stop();
        loop_handle.join().unwrap();
    }

    #[test]
    fn store_mismatch_is_incompatible() {
        let tmp = tempfile::tempdir().unwrap();
        let ep = Endpoint::for_session(tmp.path(), "store-mismatch");
        let expected = ExpectedIdentity {
            build: "test-build".into(),
            alef_home: Some(crate::protocol::message::OsBytes::Unix(b"/y".to_vec())),
            alef_app_home: None,
        };
        let (server, loop_handle) = start_server(&ep, test_token(4), Some(expected));
        // The client reports a DIFFERENT store value than the broker expects.
        let mut hello_msg = hello("test-build");
        #[cfg(windows)]
        {
            hello_msg.alef_home = Some(crate::protocol::message::OsBytes::Windows(vec![0x2f]));
        }
        #[cfg(unix)]
        {
            hello_msg.alef_home = Some(crate::protocol::message::OsBytes::Unix(b"/x".to_vec()));
        }
        let err = Client::connect_with_hello(&ep, &test_token(4), hello_msg).unwrap_err();
        assert!(
            matches!(err, ConnectError::Incompatible(_)),
            "expected Incompatible, got {err}"
        );
        server.stop();
        loop_handle.join().unwrap();
    }

    #[test]
    fn rate_limit_rejects_burst() {
        let tmp = tempfile::tempdir().unwrap();
        let ep = Endpoint::for_session(tmp.path(), "rate-limit");
        let (server, loop_handle) = start_server(&ep, test_token(5), None);
        let mut client = Client::connect_with_hello(&ep, &test_token(5), hello("b")).unwrap();
        // TokenBucket default: 200 frames/s, capacity 200. Fire past capacity
        // without reading replies; the server must answer Reject(RateLimited).
        for i in 0..260 {
            let _ = client.send(&ClientMsg::Query(Query {
                request_id: format!("r{i}"),
            }));
        }
        // The replies to the first queries come before the Reject: read up to it.
        loop {
            match client.recv() {
                Ok(BrokerMsg::Status(_)) => continue,
                Ok(BrokerMsg::Reject(r)) => {
                    assert_eq!(r.reason, RejectReason::RateLimited);
                    break;
                }
                Err(ConnectError::Io(e)) => {
                    // Some platforms deliver the close before the reject frame.
                    assert_ne!(e.kind(), std::io::ErrorKind::WouldBlock);
                    break;
                }
                other => panic!("unexpected: {other:?}"),
            }
        }
        server.stop();
        loop_handle.join().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn too_large_frame_closes_connection() {
        use std::io::{Read, Write};
        let tmp = tempfile::tempdir().unwrap();
        let ep = Endpoint::for_session(tmp.path(), "too-large");
        let token = test_token(6);
        let (server, loop_handle) = start_server(&ep, Arc::clone(&token), None);
        // Raw handshake with correct MAC, then an oversized declared length.
        let mut raw = crate::ipc::unix::connect_with_timeout(
            &ep.unix_socket,
            std::time::Duration::from_secs(2),
        )
        .unwrap();
        let mut frames = crate::ipc::FrameReader::new();
        let ns = frames.read_frame(&mut raw).unwrap();
        let nc = [9u8; crate::protocol::auth::NONCE_LEN];
        let mut n = [0u8; crate::protocol::auth::NONCE_LEN];
        n.copy_from_slice(&ns);
        let mac = crate::protocol::auth::mac_client(&token, &n, &nc);
        let mut reply = Vec::new();
        reply.extend_from_slice(&nc);
        reply.extend_from_slice(&mac);
        crate::ipc::write_frame(&mut raw, &reply).unwrap();
        let sm = frames.read_frame(&mut raw).unwrap();
        let mut server_mac = [0u8; 32];
        server_mac.copy_from_slice(&sm);
        assert!(crate::protocol::auth::client_verify_server(
            &token,
            &nc,
            &n,
            &server_mac
        ));
        // Declared length beyond the hard cap: server must close (fail-closed).
        raw.write_all(&0xFFFF_FFFFu32.to_le_bytes()).unwrap();
        raw.flush().unwrap();
        raw.set_read_timeout(Some(std::time::Duration::from_secs(2)))
            .unwrap();
        let mut sink = [0u8; 16];
        let reached_eof = loop {
            match raw.read(&mut sink) {
                Ok(0) => break true,
                Ok(_) => continue,
                Err(e) => break e.kind() != std::io::ErrorKind::WouldBlock,
            }
        };
        assert!(reached_eof, "server must close after TooLarge");
        server.stop();
        loop_handle.join().unwrap();
    }

    #[test]
    fn stop_unblocks_pending_accept() {
        // Bind a server, do NOT connect, then stop: the wake-connection must
        // unblock the accept loop and the thread must terminate. `stop` is
        // callable from the main thread because `run` holds only a clone of
        // the Arc — no sleep-based synchronization.
        let tmp = tempfile::tempdir().unwrap();
        let ep = Endpoint::for_session(tmp.path(), "stop-unblocks");
        let (server, loop_handle) = start_server(&ep, test_token(7), None);
        server.stop();
        loop_handle.join().unwrap();
        server.join();
    }
}
