// SPDX-License-Identifier: MIT OR Apache-2.0
//! Document-scoped sessions, bearer validation and call budgeting.
use super::{resources::ResourceTable, streams::StreamHub};
use crate::{
    error::{AlefError, ErrorCode},
    ids::SessionId,
    protocol::call::Limits,
    security::permissions::Grants,
};
use std::{
    collections::HashMap,
    fmt,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc, Mutex,
    },
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore, TryAcquireError};

/// Runtime-provided source of session bearer tokens.
pub type TokenSource = Arc<dyn Fn() -> String + Send + Sync>;

/// Creates, validates, replaces and closes document sessions.
pub struct SessionManager {
    sessions: Mutex<HashMap<u64, Arc<Session>>>,
    next_id: Mutex<u64>,
    tokens: TokenSource,
    limits: Limits,
}
impl SessionManager {
    /// Creates a manager with an injected token source and per-session limits.
    pub fn new(tokens: TokenSource, limits: Limits) -> Self {
        Self {
            sessions: Mutex::new(HashMap::new()),
            next_id: Mutex::new(1),
            tokens,
            limits,
        }
    }
    /// Installs a fresh document session, then closes its replaced predecessor outside the lock.
    pub async fn begin_document(&self, window: u64) -> Arc<Session> {
        let (session, old) = {
            let mut sessions = self.sessions.lock().expect("session mutex poisoned");
            let session = Arc::new(self.create(window, None));
            let old = sessions.insert(window, session.clone());
            (session, old)
        };
        if let Some(old) = old {
            old.close().await;
        }
        session
    }
    /// The session of `document` in `window` (window 0 means "no window" and never has one).
    ///
    /// Documents of a window are ordered (a newer document has a larger id): the current session
    /// when it already belongs to `document`; a fresh one replacing the older (or unbound) current
    /// one; `None` when a newer document already owns the window, so a stale document cannot
    /// take its place back. Check and replacement are one step: concurrent callers of the same
    /// document get the same session.
    pub async fn session_for_document(&self, window: u64, document: u64) -> Option<Arc<Session>> {
        if window == 0 {
            return None;
        }
        let (session, old) = {
            let mut sessions = self.sessions.lock().expect("session mutex poisoned");
            let current = sessions.get(&window).filter(|s| s.is_open());
            match current.and_then(|s| s.document) {
                Some(bound) if bound == document => return current.cloned(),
                Some(bound) if bound > document => return None,
                _ => {}
            }
            let session = Arc::new(self.create(window, Some(document)));
            let old = sessions.insert(window, session.clone());
            (session, old)
        };
        if let Some(old) = old {
            old.close().await;
        }
        Some(session)
    }
    fn create(&self, window: u64, document: Option<u64>) -> Session {
        let token = (self.tokens)();
        let id = {
            let mut n = self.next_id.lock().expect("session id mutex poisoned");
            let id = SessionId(*n);
            *n = n.checked_add(1).expect("session id exhausted");
            id
        };
        Session::new(id, window, document, token, self.limits)
    }
    /// Validates a token against currently open sessions; length mismatch may be observed, count affects total latency.
    pub fn validate(&self, token: &str) -> Option<Arc<Session>> {
        self.sessions
            .lock()
            .ok()?
            .values()
            .find(|session| {
                session.is_open() && constant_time_eq(session.token.as_bytes(), token.as_bytes())
            })
            .cloned()
    }
    /// Returns the window's currently open session, if any.
    pub fn current(&self, window: u64) -> Option<Arc<Session>> {
        self.sessions
            .lock()
            .ok()?
            .get(&window)
            .cloned()
            .filter(|s| s.is_open())
    }
    /// Closes and forgets a window's current session.
    pub async fn close_window(&self, window: u64) {
        let session = self
            .sessions
            .lock()
            .expect("session mutex poisoned")
            .remove(&window);
        if let Some(session) = session {
            session.close().await;
        }
    }
    /// Closes and forgets all sessions.
    pub async fn close_all(&self) {
        let sessions = {
            let mut map = self.sessions.lock().expect("session mutex poisoned");
            map.drain().map(|(_, s)| s).collect::<Vec<_>>()
        };
        for session in sessions {
            session.close().await;
        }
    }
}

pub(crate) fn constant_time_eq(a: &[u8], b: &[u8]) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.iter().zip(b) {
        diff |= x ^ y;
    }
    diff == 0
}

/// One document's resources, streams and concurrent-call budget.
pub struct Session {
    id: SessionId,
    window: u64,
    document: Option<u64>,
    token: String,
    resources: ResourceTable,
    streams: StreamHub,
    grants: Arc<Grants>,
    calls: Arc<Semaphore>,
    open: AtomicBool,
}
impl fmt::Debug for Session {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Session")
            .field("id", &self.id)
            .field("window", &self.window)
            .field("open", &self.is_open())
            .finish_non_exhaustive()
    }
}
impl Session {
    fn new(
        id: SessionId,
        window: u64,
        document: Option<u64>,
        token: String,
        limits: Limits,
    ) -> Self {
        Self {
            id,
            window,
            document,
            token,
            resources: ResourceTable::new(limits.max_resources_per_session),
            streams: StreamHub::new(limits.stream_window, limits.chunk_size),
            grants: Arc::new(Grants::new()),
            calls: Arc::new(Semaphore::new(limits.max_concurrent_calls_per_session)),
            open: AtomicBool::new(true),
        }
    }
    /// Returns the session id.
    pub fn id(&self) -> SessionId {
        self.id
    }
    /// Returns the owning window id.
    pub fn window(&self) -> u64 {
        self.window
    }
    /// Returns this session's runtime path grants (dialogs, drops).
    pub fn grants(&self) -> Arc<Grants> {
        Arc::clone(&self.grants)
    }
    /// Returns the secret bearer token; never log it or embed it in Debug output.
    pub fn token(&self) -> &str {
        &self.token
    }
    /// Returns this session's resource table.
    pub fn resources(&self) -> &ResourceTable {
        &self.resources
    }
    /// Returns this session's stream hub.
    pub fn streams(&self) -> &StreamHub {
        &self.streams
    }
    /// Reports whether the session remains open.
    pub fn is_open(&self) -> bool {
        self.open.load(Ordering::Acquire)
    }
    /// Takes one concurrent call slot; the permit releases on drop.
    pub fn try_begin_call(&self) -> Result<CallPermit, AlefError> {
        if !self.is_open() {
            return Err(AlefError::new(ErrorCode::Closed, "session closed"));
        }
        self.calls
            .clone()
            .try_acquire_owned()
            .map(CallPermit::from)
            .map_err(|e| {
                AlefError::new(
                    match e {
                        TryAcquireError::NoPermits => ErrorCode::Busy,
                        TryAcquireError::Closed => ErrorCode::Closed,
                    },
                    "call slot unavailable",
                )
            })
    }
    /// Closes streams and then resources once.
    pub async fn close(&self) {
        if self.open.swap(false, Ordering::AcqRel) {
            self.streams.close_all();
            self.calls.close();
            self.resources.close_all().await;
        }
    }
}
/// Call slot held for the duration of a command.
#[derive(Debug)]
pub struct CallPermit {
    _permit: OwnedSemaphorePermit,
}
impl From<OwnedSemaphorePermit> for CallPermit {
    fn from(p: OwnedSemaphorePermit) -> Self {
        Self { _permit: p }
    }
}

#[cfg(test)]
mod tests {
    use super::super::resources::Resource;
    use super::*;
    use crate::security::{
        manifest::Permissions,
        permissions::{PathVars, Permission, PermissionSet},
    };
    use std::{
        collections::VecDeque,
        future::Future,
        pin::Pin,
        sync::{Arc, Mutex},
    };
    struct Recording {
        log: Arc<Mutex<Vec<&'static str>>>,
    }
    impl Resource for Recording {
        fn close(self: Box<Self>) -> Pin<Box<dyn Future<Output = ()> + Send>> {
            Box::pin(async move { self.log.lock().expect("log").push("recording") })
        }
    }
    use std::time::Duration;
    use tokio::time::timeout;
    fn source() -> TokenSource {
        let tokens = Arc::new(Mutex::new(VecDeque::from([
            "tok-1".to_string(),
            "tok-2".to_string(),
            "tok-3".to_string(),
        ])));
        Arc::new(move || tokens.lock().expect("tokens").pop_front().expect("token"))
    }
    async fn bounded<T>(future: impl std::future::Future<Output = T>) -> T {
        timeout(Duration::from_secs(10), future)
            .await
            .expect("must not time out")
    }
    #[tokio::test(flavor = "multi_thread", worker_threads = 4)]
    async fn concurrent_callers_of_one_document_share_one_session() {
        let counter = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let tokens: TokenSource =
            Arc::new(move || format!("tok-{}", counter.fetch_add(1, Ordering::SeqCst)));
        let manager = Arc::new(SessionManager::new(tokens, Limits::default()));
        let tasks: Vec<_> = (0..16)
            .map(|_| {
                let manager = manager.clone();
                tokio::spawn(async move { manager.session_for_document(1, 7).await })
            })
            .collect();
        let mut ids = std::collections::HashSet::new();
        for task in tasks {
            let session = bounded(task).await.expect("task").expect("session");
            assert!(
                session.is_open(),
                "a caller was handed a session that was already replaced"
            );
            ids.insert(session.id());
        }
        assert_eq!(ids.len(), 1, "one document, one session");
    }
    #[tokio::test]
    async fn documents_are_ordered_and_window_zero_has_none() {
        let counter = std::sync::atomic::AtomicUsize::new(0);
        let tokens: TokenSource =
            Arc::new(move || format!("tok-{}", counter.fetch_add(1, Ordering::SeqCst)));
        let m = SessionManager::new(tokens, Limits::default());
        assert!(bounded(m.session_for_document(0, 1)).await.is_none());
        let first = bounded(m.session_for_document(1, 5)).await.expect("first");
        let same = bounded(m.session_for_document(1, 5)).await.expect("same");
        assert_eq!(first.id(), same.id());
        let newer = bounded(m.session_for_document(1, 6)).await.expect("newer");
        assert_ne!(first.id(), newer.id());
        assert!(!first.is_open(), "the replaced session is closed");
        assert!(
            bounded(m.session_for_document(1, 5)).await.is_none(),
            "stale document"
        );
        assert!(newer.is_open());
        let legacy = bounded(m.begin_document(2)).await;
        let bound = bounded(m.session_for_document(2, 1)).await.expect("bound");
        assert_ne!(legacy.id(), bound.id(), "an unbound session is replaceable");
    }
    #[test]
    fn constant_time_eq_contract() {
        assert!(constant_time_eq(b"abc", b"abc"));
        assert!(!constant_time_eq(b"abc", b"abd"));
        assert!(!constant_time_eq(b"a", b"ab"));
        assert!(constant_time_eq(b"", b""));
    }
    #[tokio::test]
    async fn validates_and_rejects_stale_tokens() {
        let m = SessionManager::new(source(), Limits::default());
        let s = bounded(m.begin_document(1)).await;
        assert_eq!(m.validate("tok-1").expect("valid").id(), s.id());
        assert!(m.validate("nope").is_none());
        assert!(m.validate("").is_none());
        bounded(m.close_window(1)).await;
        assert!(m.validate("tok-1").is_none());
    }
    #[tokio::test]
    async fn current_returns_open_session_per_window() {
        let m = SessionManager::new(source(), Limits::default());
        let session = bounded(m.begin_document(1)).await;
        assert_eq!(m.current(1).expect("current").id(), session.id());
        assert!(m.current(2).is_none());
        assert_eq!(
            m.validate(session.token()).expect("valid").id(),
            session.id()
        );
        bounded(m.close_window(1)).await;
        assert!(m.current(1).is_none());
    }
    #[tokio::test]
    async fn grants_are_per_session() {
        // Unit tests get no CARGO_TARGET_TMPDIR; the test binary lives in <target>/<profile>/deps.
        let binary = std::env::current_exe().expect("test binary path");
        let dir = tempfile::Builder::new()
            .prefix("alef-session-")
            .tempdir_in(binary.parent().expect("deps directory"))
            .expect("tempdir");
        let file = dir.path().join("file");
        std::fs::write(&file, b"data").expect("write");
        let policy: Permissions = serde_json::from_value(serde_json::json!({"fs":{"read":[],"write":[]},"cli":{"exec":[]},"net":{"http":[],"socket":[]},"shell":{"openExternal":[]},"clipboard":{"read":false},"shortcut":{"global":false},"secrets":false,"app":{"env":[]}})).expect("permissions");
        let path = dir.path().to_path_buf();
        let permission_set = Arc::new(
            PermissionSet::from_manifest(
                &policy,
                &PathVars {
                    app_data: path.clone(),
                    app_config: path.clone(),
                    app_cache: path.clone(),
                    home: path.clone(),
                    documents: path.clone(),
                    downloads: path.clone(),
                    desktop: path.clone(),
                    temp: path.clone(),
                    app: path,
                },
            )
            .expect("permission set"),
        );
        let m = SessionManager::new(source(), Limits::default());
        let s1 = bounded(m.begin_document(1)).await;
        let s2 = bounded(m.begin_document(2)).await;
        s1.grants().grant_write(&file).expect("grant");
        let target = file.to_string_lossy();
        assert!(permission_set
            .authorize_path(Permission::FsWrite, Some(&target), &s1.grants())
            .is_ok());
        assert_eq!(
            permission_set
                .authorize_path(Permission::FsWrite, Some(&target), &s2.grants())
                .expect_err("not granted")
                .code,
            ErrorCode::PermissionDenied
        );
        drop(dir);
    }
    #[tokio::test]
    async fn reload_closes_replaced_session() {
        let m = SessionManager::new(source(), Limits::default());
        let a = bounded(m.begin_document(1)).await;
        let log = Arc::new(Mutex::new(Vec::new()));
        a.resources()
            .insert(Box::new(Recording { log: log.clone() }))
            .expect("resource");
        let (w, id) = a.streams.open_outgoing();
        let mut reader = a.streams.reader(id).expect("reader");
        let b = bounded(m.begin_document(1)).await;
        assert_ne!(a.id(), b.id());
        assert!(m.validate("tok-1").is_none());
        assert_eq!(m.validate("tok-2").expect("new token").id(), b.id());
        assert!(!a.is_open());
        assert!(
            matches!(bounded(reader.next_frame()).await,Some(crate::protocol::frame::Frame::Error(e)) if e.code==ErrorCode::Closed)
        );
        assert_eq!(bounded(reader.next_frame()).await, None);
        assert_eq!(a.resources().len(), 0);
        assert_eq!(*log.lock().expect("log"), vec!["recording"]);
        assert!(a.streams().is_closed());
        drop(w);
    }
    #[tokio::test]
    async fn windows_have_independent_sessions_and_ids() {
        let m = SessionManager::new(source(), Limits::default());
        let a = bounded(m.begin_document(1)).await;
        let b = bounded(m.begin_document(2)).await;
        assert_ne!(a.id(), b.id());
        assert!(m.validate("tok-1").is_some() && m.validate("tok-2").is_some());
        bounded(m.close_window(2)).await;
        assert!(a.is_open() && m.validate("tok-1").is_some());
    }
    #[tokio::test]
    async fn call_limit_releases_and_close_rejects() {
        let limits = Limits {
            max_concurrent_calls_per_session: 2,
            ..Limits::default()
        };
        let m = SessionManager::new(source(), limits);
        let s = bounded(m.begin_document(1)).await;
        let a = s.try_begin_call().expect("permit");
        let _b = s.try_begin_call().expect("permit");
        assert_eq!(s.try_begin_call().expect_err("busy").code, ErrorCode::Busy);
        drop(a);
        drop(s.try_begin_call().expect("released"));
        bounded(s.close()).await;
        assert_eq!(
            s.try_begin_call().expect_err("closed").code,
            ErrorCode::Closed
        );
    }
    #[tokio::test]
    async fn close_all_closes_all_windows() {
        let m = SessionManager::new(source(), Limits::default());
        let sessions = vec![
            bounded(m.begin_document(1)).await,
            bounded(m.begin_document(2)).await,
            bounded(m.begin_document(3)).await,
        ];
        let log = Arc::new(Mutex::new(Vec::new()));
        sessions[0]
            .resources()
            .insert(Box::new(Recording { log: log.clone() }))
            .expect("resource");
        bounded(m.close_all()).await;
        for s in sessions {
            assert!(!s.is_open() && s.streams.is_closed() && s.resources.is_closed());
        }
        for token in ["tok-1", "tok-2", "tok-3"] {
            assert!(m.validate(token).is_none());
        }
        assert_eq!(*log.lock().expect("log"), vec!["recording"]);
    }
}
