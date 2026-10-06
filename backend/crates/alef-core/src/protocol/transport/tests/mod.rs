// SPDX-License-Identifier: MIT OR Apache-2.0
//! Transport tests: shared fixture. Every await is bounded so a hang is a failure, never a stuck run.
mod auth;
mod call;
mod lifecycle;
mod streams;

use super::*;
use crate::{
    registry::command::Reply,
    security::{
        manifest::Permissions,
        permissions::{PathVars, Permission, PermissionSet},
    },
    session::{resources::Resource, streams::IncomingReader, Session},
};
use bytes::Bytes;
use serde_json::{json, Value};
use std::{
    future::Future,
    pin::Pin,
    sync::{
        atomic::{AtomicBool, AtomicUsize, Ordering},
        Mutex,
    },
    time::Duration,
};
use tokio::{sync::Semaphore, task::JoinHandle};

pub(super) const BOOTSTRAP: &str = "bootstrap-secret-0001";
pub(super) const WINDOW: u64 = 1;

pub(super) async fn bounded<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(10), future)
        .await
        .expect("timed out")
}

/// Observation points shared between the test and the registered handlers.
pub(super) struct Probe {
    /// Handler bodies that ran to the point of doing work.
    pub handled: AtomicUsize,
    /// One permit per handler that entered `test.slow`.
    pub entered: Semaphore,
    /// One permit lets one `test.slow` handler finish.
    pub release: Semaphore,
    /// Set when the `test.hang` future is dropped.
    pub hang_dropped: AtomicBool,
    /// Page-to-runtime readers opened by `test.up`.
    pub readers: Mutex<Vec<IncomingReader>>,
    /// Producer tasks started by `test.out`/`test.flood`.
    pub producers: Mutex<Vec<JoinHandle<Result<(), AlefError>>>>,
    /// Set when a `test.resource` resource is closed.
    pub resource_closed: Arc<AtomicBool>,
}

impl Probe {
    fn new() -> Self {
        Self {
            handled: AtomicUsize::new(0),
            entered: Semaphore::new(0),
            release: Semaphore::new(0),
            hang_dropped: AtomicBool::new(false),
            readers: Mutex::new(Vec::new()),
            producers: Mutex::new(Vec::new()),
            resource_closed: Arc::new(AtomicBool::new(false)),
        }
    }
}

struct Recorder(Arc<AtomicBool>);

impl Resource for Recorder {
    fn close(self: Box<Self>) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        Box::pin(async move { self.0.store(true, Ordering::SeqCst) })
    }
}

struct DropFlag(Arc<Probe>);

impl Drop for DropFlag {
    fn drop(&mut self) {
        self.0.hang_dropped.store(true, Ordering::SeqCst);
    }
}

fn closed_permissions() -> Arc<PermissionSet> {
    let policy: Permissions = serde_json::from_value(json!({
        "fs": {"read": [], "write": []},
        "cli": {"exec": []},
        "net": {"http": [], "socket": []},
        "shell": {"openExternal": []},
        "clipboard": {"read": false},
        "shortcut": {"global": false},
        "secrets": false,
        "app": {"env": []},
    }))
    .expect("policy");
    let root = std::env::current_dir().expect("cwd");
    let vars = PathVars {
        app_data: root.clone(),
        app_config: root.clone(),
        app_cache: root.clone(),
        home: root.clone(),
        documents: root.clone(),
        downloads: root.clone(),
        desktop: root.clone(),
        temp: root.clone(),
        app: root,
    };
    Arc::new(PermissionSet::from_manifest(&policy, &vars).expect("permission set"))
}

fn register(registry: &mut Registry, probe: &Arc<Probe>) {
    registry
        .command::<Value>("test.echo")
        .expect("name")
        .handler(|_, args| async move { Ok(Reply::Json(args)) })
        .expect("register");
    registry
        .command::<Value>("test.bin")
        .expect("name")
        .handler(|ctx, _| async move {
            let body = ctx.body().cloned().unwrap_or_default();
            Ok(Reply::Bytes(body))
        })
        .expect("register");
    registry
        .command::<Value>("test.panic")
        .expect("name")
        .handler(|_, _| async move { panic!("panic-text-must-not-leak") })
        .expect("register");
    let p = probe.clone();
    registry
        .command::<Value>("test.slow")
        .expect("name")
        .handler(move |_, _| {
            let p = p.clone();
            async move {
                p.entered.add_permits(1);
                p.release.acquire().await.expect("release").forget();
                Ok(Reply::Json(Value::Null))
            }
        })
        .expect("register");
    let p = probe.clone();
    registry
        .command::<Value>("test.hang")
        .expect("name")
        .handler(move |_, _| {
            let flag = DropFlag(p.clone());
            let p = p.clone();
            async move {
                let _flag = flag;
                p.entered.add_permits(1);
                std::future::pending::<()>().await;
                Ok(Reply::Json(Value::Null))
            }
        })
        .expect("register");
    let p = probe.clone();
    registry
        .command::<PathArgs>("test.denied")
        .expect("name")
        .permission(Permission::FsRead, |args| Some(args.path.clone()))
        .handler(move |_, _| {
            p.handled.fetch_add(1, Ordering::SeqCst);
            async { Ok(Reply::Json(Value::Null)) }
        })
        .expect("register");
    let p = probe.clone();
    registry
        .command::<Value>("test.out")
        .expect("name")
        .handler(move |ctx, _| {
            let p = p.clone();
            async move {
                let (writer, id) = ctx.streams().open_outgoing();
                let task = tokio::spawn(async move {
                    writer.send_json(json!({"hello": 1})).await?;
                    writer.send_binary(Bytes::from_static(b"abc")).await?;
                    writer.end();
                    Ok(())
                });
                p.producers.lock().expect("producers").push(task);
                Ok(Reply::Stream(id))
            }
        })
        .expect("register");
    let p = probe.clone();
    registry
        .command::<FloodArgs>("test.flood")
        .expect("name")
        .handler(move |ctx, args| {
            let p = p.clone();
            async move {
                let (writer, id) = ctx.streams().open_outgoing();
                let task = tokio::spawn(async move {
                    let mut sent = 0;
                    while sent < args.total {
                        writer
                            .send_binary(Bytes::from(vec![7u8; args.piece]))
                            .await?;
                        sent += args.piece;
                    }
                    writer.end();
                    Ok(())
                });
                p.producers.lock().expect("producers").push(task);
                Ok(Reply::Stream(id))
            }
        })
        .expect("register");
    let p = probe.clone();
    registry
        .command::<Value>("test.up")
        .expect("name")
        .handler(move |ctx, _| {
            let p = p.clone();
            async move {
                let (reader, id) = ctx.streams().open_incoming_reader();
                p.readers.lock().expect("readers").push(reader);
                Ok(Reply::Stream(id))
            }
        })
        .expect("register");
    let p = probe.clone();
    registry
        .command::<Value>("test.resource")
        .expect("name")
        .handler(move |ctx, _| {
            let closed = p.resource_closed.clone();
            async move {
                ctx.resources().insert(Box::new(Recorder(closed)))?;
                Ok(Reply::Json(Value::Null))
            }
        })
        .expect("register");
}

#[derive(serde::Deserialize)]
struct PathArgs {
    path: String,
}

#[derive(serde::Deserialize)]
struct FloodArgs {
    total: usize,
    piece: usize,
}

pub(super) struct Fixture {
    pub transport: Arc<Transport>,
    pub sessions: Arc<SessionManager>,
    pub probe: Arc<Probe>,
}

pub(super) fn config(limits: Limits) -> TransportConfig {
    TransportConfig {
        bootstrap_token: BOOTSTRAP.into(),
        allowed_origins: Vec::new(),
        runtime_version: "9.9.9".into(),
        limits,
    }
}

pub(super) fn fixture(config: TransportConfig) -> Fixture {
    let probe = Arc::new(Probe::new());
    let mut registry = Registry::default();
    register(&mut registry, &probe);
    let counter = AtomicUsize::new(0);
    let source: crate::session::TokenSource =
        Arc::new(move || format!("tok-{}", counter.fetch_add(1, Ordering::SeqCst)));
    let sessions = Arc::new(SessionManager::new(source, config.limits));
    let transport = Transport::new(config, registry, sessions.clone(), closed_permissions())
        .expect("transport");
    Fixture {
        transport: Arc::new(transport),
        sessions,
        probe,
    }
}

pub(super) async fn open(fixture: &Fixture, window: u64) -> Arc<Session> {
    bounded(fixture.sessions.begin_document(window)).await
}

pub(super) fn request(method: Method, path: &str) -> TransportRequest {
    TransportRequest {
        method,
        path: path.into(),
        headers: Vec::new(),
        body: Bytes::new(),
        window: WINDOW,
        document: None,
    }
}

impl TransportRequest {
    pub(super) fn token(self, token: &str) -> Self {
        self.header_set("authorization", &format!("Bearer {token}"))
    }

    pub(super) fn header_set(mut self, name: &str, value: &str) -> Self {
        self.headers.push((name.into(), value.into()));
        self
    }

    pub(super) fn on_window(mut self, window: u64) -> Self {
        self.window = window;
        self
    }

    pub(super) fn in_document(mut self, document: u64) -> Self {
        self.document = Some(document);
        self
    }

    pub(super) fn json(mut self, value: &Value) -> Self {
        self.body = serde_json::to_vec(value).expect("json").into();
        self.header_set("content-type", "application/json")
    }

    pub(super) fn octets(mut self, body: Bytes, args: &Value) -> Self {
        self.body = body;
        let encoded = percent_encode(&serde_json::to_string(args).expect("args"));
        self.header_set("content-type", "application/octet-stream")
            .header_set("x-alef-args", &encoded)
    }
}

fn percent_encode(text: &str) -> String {
    text.bytes()
        .map(|b| match b {
            b'0'..=b'9' | b'a'..=b'z' | b'A'..=b'Z' => char::from(b).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

/// POST `call/<name>` with a JSON body.
pub(super) fn call(name: &str, token: &str, args: &Value) -> TransportRequest {
    request(Method::Post, &format!("call/{name}"))
        .token(token)
        .json(args)
}

pub(super) async fn send(fixture: &Fixture, request: TransportRequest) -> TransportResponse {
    bounded(fixture.transport.handle(request)).await
}

const DENIED: &str = r#"{"code":"PERMISSION_DENIED","message":"permission denied"}"#;

/// The one uniform denial: same status and same body whatever the reason.
pub(super) fn assert_denied(response: &TransportResponse, what: &str) {
    assert_eq!(response.status, 403, "{what}");
    assert_eq!(body(response).as_ref(), DENIED.as_bytes(), "{what}");
}

pub(super) fn body(response: &TransportResponse) -> &Bytes {
    match &response.body {
        ResponseBody::Bytes(bytes) => bytes,
        other => panic!("expected a buffered body, got {other:?}"),
    }
}

pub(super) fn body_json(response: &TransportResponse) -> Value {
    serde_json::from_slice(body(response)).expect("json body")
}
