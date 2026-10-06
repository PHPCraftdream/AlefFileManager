// SPDX-License-Identifier: MIT OR Apache-2.0
//! Public command registry integration tests.
use alef_core::{
    ids::StreamId,
    protocol::Limits,
    registry::{
        command::Reply,
        context::{CallContext, CancelHandle},
        dispatch::Registry,
    },
    security::{
        manifest::Permissions,
        permissions::{PathVars, Permission, PermissionSet},
    },
    session::{session::Session, SessionManager},
    AlefError, ErrorCode,
};
use serde::Deserialize;
use std::{
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
    time::Duration,
};
#[derive(Deserialize)]
struct Args {
    #[serde(rename = "n")]
    _n: u32,
    #[serde(default)]
    path: String,
}
fn tmp() -> std::path::PathBuf {
    std::path::PathBuf::from(env!("CARGO_TARGET_TMPDIR"))
}
fn perms(read: Vec<String>, write: Vec<String>) -> Arc<PermissionSet> {
    let p:Permissions=serde_json::from_value(serde_json::json!({"fs":{"read":read,"write":write},"cli":{"exec":[]},"net":{"http":[],"socket":[]},"shell":{"openExternal":[]},"clipboard":{"read":false},"shortcut":{"global":false},"secrets":false,"app":{"env":[]}})).unwrap();
    Arc::new(
        PermissionSet::from_manifest(
            &p,
            &PathVars {
                app_data: tmp(),
                app_config: tmp(),
                app_cache: tmp(),
                home: tmp(),
                documents: tmp(),
                downloads: tmp(),
                desktop: tmp(),
                temp: tmp(),
                app: tmp(),
            },
        )
        .unwrap(),
    )
}
fn context(s: Arc<Session>) -> CallContext {
    CallContext::new(s, perms(vec![], vec![]))
}
fn token_source() -> alef_core::session::session::TokenSource {
    let counter = Arc::new(AtomicUsize::new(0));
    Arc::new(move || format!("tok-{}", counter.fetch_add(1, Ordering::SeqCst)))
}
async fn session() -> Arc<Session> {
    timed(SessionManager::new(token_source(), Limits::default()).begin_document(1))
        .await
        .expect("session creation must not time out")
}
fn timed<F: std::future::Future>(
    f: F,
) -> impl std::future::Future<Output = Result<F::Output, tokio::time::error::Elapsed>> {
    tokio::time::timeout(Duration::from_secs(10), f)
}
#[tokio::test]
async fn unknown() {
    let r = Registry::default();
    assert_eq!(
        timed(r.dispatch("foo.bar", context(session().await), serde_json::json!({})))
            .await
            .unwrap()
            .err()
            .unwrap()
            .code,
        ErrorCode::NotFound
    )
}
#[tokio::test]
async fn invalid_names_and_runtime() {
    let mut r = Registry::default();
    for n in [
        "",
        "foo",
        "foo.",
        ".foo",
        "foo..bar",
        "Foo.bar",
        "foo.Bar",
        "foo bar.x",
        "9foo.x",
    ] {
        assert_eq!(
            r.command::<Args>(n).err().unwrap().code,
            ErrorCode::InvalidArgument
        )
    }
    assert!(r
        .command::<Args>("runtime.hello")
        .err()
        .unwrap()
        .message
        .contains("reserved"));
    r.register_runtime::<Args>("runtime.hello")
        .unwrap()
        .handler(|_, _| async { Ok(Reply::Json(serde_json::json!(true))) })
        .unwrap();
    assert!(timed(r.dispatch(
        "runtime.hello",
        context(session().await),
        serde_json::json!({"n":1})
    ))
    .await
    .unwrap()
    .is_ok())
}
#[tokio::test]
async fn invalid_args_safe() {
    let mut r = Registry::default();
    let c = Arc::new(AtomicUsize::new(0));
    let cc = c.clone();
    r.command::<Args>("test.args")
        .unwrap()
        .handler(move |_, _| {
            cc.fetch_add(1, Ordering::SeqCst);
            async { Ok(Reply::Json(serde_json::json!(null))) }
        })
        .unwrap();
    let e = timed(r.dispatch(
        "test.args",
        context(session().await),
        serde_json::json!("secret"),
    ))
    .await
    .unwrap()
    .err()
    .unwrap();
    assert_eq!(e.code, ErrorCode::InvalidArgument);
    assert!(!e.message.contains("secret"));
    assert!(e.message.len() <= 200);
    assert_eq!(c.load(Ordering::SeqCst), 0)
}
#[tokio::test]
async fn missing_permission_blocks() {
    let mut r = Registry::default();
    let c = Arc::new(AtomicUsize::new(0));
    let cc = c.clone();
    r.command::<Args>("test.deny")
        .unwrap()
        .permission(Permission::FsRead, |_| {
            Some(tmp().join("private/file").to_string_lossy().into_owned())
        })
        .handler(move |_, _| {
            cc.fetch_add(1, Ordering::SeqCst);
            async { Ok(Reply::Json(serde_json::json!(null))) }
        })
        .unwrap();
    let e = timed(r.dispatch(
        "test.deny",
        context(session().await),
        serde_json::json!({"n":1}),
    ))
    .await
    .unwrap()
    .err()
    .unwrap();
    assert_eq!(e.code, ErrorCode::PermissionDenied);
    assert_eq!(e.details.unwrap()["permission"], "fs.read");
    assert_eq!(c.load(Ordering::SeqCst), 0)
}
#[tokio::test]
async fn out_of_scope_blocks() {
    let mut r = Registry::default();
    let c = Arc::new(AtomicUsize::new(0));
    let cc = c.clone();
    let ctx = CallContext::new(
        session().await,
        perms(vec![format!("{}/allowed/**", tmp().display())], vec![]),
    );
    r.command::<Args>("test.scope")
        .unwrap()
        .permission(Permission::FsRead, |_| {
            Some(tmp().join("else/file").to_string_lossy().into_owned())
        })
        .handler(move |_, _| {
            cc.fetch_add(1, Ordering::SeqCst);
            async { Ok(Reply::Json(serde_json::json!(null))) }
        })
        .unwrap();
    assert_eq!(
        timed(r.dispatch("test.scope", ctx, serde_json::json!({"n":1})))
            .await
            .unwrap()
            .err()
            .unwrap()
            .code,
        ErrorCode::PermissionDenied
    );
    assert_eq!(c.load(Ordering::SeqCst), 0)
}
#[tokio::test]
async fn grant_allows_write() {
    let dir = tempfile::Builder::new()
        .prefix("alef-registry-")
        .tempdir_in(env!("CARGO_TARGET_TMPDIR"))
        .unwrap();
    let path = dir.path().join("data");
    std::fs::write(&path, b"ok").unwrap();
    let session = session().await;
    session.grants().grant_write(&path).unwrap();
    let ctx = CallContext::new(session, perms(vec![], vec![]));
    let mut r = Registry::default();
    r.command::<Args>("test.write")
        .unwrap()
        .permission(Permission::FsWrite, |a| Some(a.path.clone()))
        .handler(|_, _| async { Ok(Reply::Json(serde_json::json!({"ok":true}))) })
        .unwrap();
    let result = timed(r.dispatch(
        "test.write",
        ctx,
        serde_json::json!({"n":1,"path":path.to_string_lossy()}),
    ))
    .await
    .unwrap()
    .unwrap();
    assert!(matches!(result,Reply::Json(v) if v==serde_json::json!({"ok":true})))
}
#[tokio::test]
async fn handler_error_unchanged() {
    let mut r = Registry::default();
    r.command::<Args>("test.error")
        .unwrap()
        .handler(|_, _| async { Err(AlefError::new(ErrorCode::Closed, "gone")) })
        .unwrap();
    let e = timed(r.dispatch(
        "test.error",
        context(session().await),
        serde_json::json!({"n":1}),
    ))
    .await
    .unwrap()
    .err()
    .unwrap();
    assert_eq!((e.code, e.message.as_str()), (ErrorCode::Closed, "gone"))
}
#[tokio::test]
async fn duplicate_rejected() {
    let mut r = Registry::default();
    r.command::<Args>("test.same")
        .unwrap()
        .handler(|_, _| async { Ok(Reply::Json(serde_json::json!(null))) })
        .unwrap();
    assert_eq!(
        r.command::<Args>("test.same").err().unwrap().code,
        ErrorCode::AlreadyExists
    )
}
#[tokio::test]
async fn concurrent_dispatch() {
    // every handler waits at a barrier for all 100: it can only open if all dispatches are in flight at once
    const N: usize = 100;
    let mut r = Registry::default();
    let barrier = Arc::new(tokio::sync::Barrier::new(N));
    let c = Arc::new(AtomicUsize::new(0));
    let cc = c.clone();
    r.command::<Args>("test.concurrent")
        .unwrap()
        .handler(move |_, _| {
            let barrier = barrier.clone();
            let cc = cc.clone();
            async move {
                barrier.wait().await;
                cc.fetch_add(1, Ordering::SeqCst);
                Ok(Reply::Json(serde_json::json!(true)))
            }
        })
        .unwrap();
    let session = session().await;
    let tasks: Vec<_> = (0..N)
        .map(|_| {
            let r = r.clone();
            let session = session.clone();
            tokio::spawn(async move {
                r.dispatch(
                    "test.concurrent",
                    context(session),
                    serde_json::json!({"n":1}),
                )
                .await
                .is_ok()
            })
        })
        .collect();
    for task in tasks {
        assert!(timed(task).await.expect("no deadlock").unwrap());
    }
    assert_eq!(c.load(Ordering::SeqCst), N)
}
#[tokio::test]
async fn default_context_is_not_cancelled_and_dropping_the_callers_sender_is() {
    let ctx = context(session().await);
    assert!(!ctx.cancel().is_cancelled());
    assert!(
        tokio::time::timeout(Duration::from_millis(100), ctx.cancel().cancelled())
            .await
            .is_err(),
        "a default context must stay pending, not resolve at once"
    );
    let (sender, handle) = CancelHandle::channel();
    let bound = context(session().await).with_cancel(handle);
    assert!(
        tokio::time::timeout(Duration::from_millis(50), bound.cancel().cancelled())
            .await
            .is_err()
    );
    sender.cancel();
    assert!(timed(bound.cancel().cancelled()).await.is_ok());
    let (sender, handle) = CancelHandle::channel();
    let abandoned = context(session().await).with_cancel(handle);
    drop(sender);
    assert!(
        timed(abandoned.cancel().cancelled()).await.is_ok(),
        "caller gone means cancelled"
    );
}
#[tokio::test]
async fn replies_intact() {
    let mut r = Registry::default();
    r.command::<Args>("test.bytes")
        .unwrap()
        .handler(|_, _| async { Ok(Reply::Bytes(bytes::Bytes::from_static(b"x"))) })
        .unwrap();
    assert!(
        matches!(timed(r.dispatch("test.bytes",context(session().await),serde_json::json!({"n":1}))).await.unwrap().unwrap(),Reply::Bytes(b) if b.as_ref()==b"x")
    );
    let mut r = Registry::default();
    r.command::<Args>("test.stream")
        .unwrap()
        .handler(|_, _| async { Ok(Reply::Stream(StreamId(9))) })
        .unwrap();
    assert!(matches!(
        timed(r.dispatch(
            "test.stream",
            context(session().await),
            serde_json::json!({"n":1})
        ))
        .await
        .unwrap()
        .unwrap(),
        Reply::Stream(StreamId(9))
    ))
}
#[tokio::test]
async fn body_reaches_handler() {
    let mut r = Registry::default();
    r.command::<Args>("test.body")
        .unwrap()
        .handler(|ctx, _| async move {
            assert_eq!(ctx.body().unwrap().as_ref(), b"hello");
            Ok(Reply::Json(serde_json::json!(true)))
        })
        .unwrap();
    assert!(timed(r.dispatch(
        "test.body",
        context(session().await).with_body(Some(bytes::Bytes::from_static(b"hello"))),
        serde_json::json!({"n":1})
    ))
    .await
    .unwrap()
    .is_ok())
}
#[tokio::test]
async fn cancellation_observed() {
    let mut r = Registry::default();
    let (sender, cancel) = CancelHandle::channel();
    sender.cancel();
    r.command::<Args>("test.cancel")
        .unwrap()
        .handler(|ctx, _| async move {
            assert!(ctx.cancel().is_cancelled());
            ctx.cancel().cancelled().await;
            Err(AlefError::new(ErrorCode::Closed, "cancelled"))
        })
        .unwrap();
    assert_eq!(
        timed(r.dispatch(
            "test.cancel",
            context(session().await).with_cancel(cancel),
            serde_json::json!({"n":1})
        ))
        .await
        .unwrap()
        .err()
        .unwrap()
        .code,
        ErrorCode::Closed
    )
}
#[tokio::test]
async fn none_permission() {
    let mut r = Registry::default();
    r.command::<Args>("test.none")
        .unwrap()
        .permission(Permission::None, |_| None)
        .handler(|_, _| async { Ok(Reply::Json(serde_json::json!(true))) })
        .unwrap();
    assert!(timed(r.dispatch(
        "test.none",
        context(session().await),
        serde_json::json!({"n":1})
    ))
    .await
    .unwrap()
    .is_ok())
}
#[tokio::test]
#[should_panic(expected = "boom")]
async fn panic_propagates() {
    let mut r = Registry::default();
    r.command::<Args>("test.panic")
        .unwrap()
        .handler(|_, _| async { panic!("boom") })
        .unwrap();
    let _ = timed(r.dispatch(
        "test.panic",
        context(session().await),
        serde_json::json!({"n":1}),
    ))
    .await
    .unwrap();
}

#[tokio::test]
async fn command_names_are_sorted() {
    let mut registry = Registry::default();
    for name in ["test.b", "test.a", "other.c"] {
        registry
            .command::<Args>(name)
            .unwrap()
            .handler(|_, _| async { Ok(Reply::Json(serde_json::json!(true))) })
            .unwrap();
    }
    assert_eq!(
        registry.command_names(),
        vec![
            "other.c".to_string(),
            "test.a".to_string(),
            "test.b".to_string()
        ]
    );
    assert!(Registry::default().command_names().is_empty());
}
