// SPDX-License-Identifier: MIT OR Apache-2.0
use super::tests::{permissions, Fake};
use super::*;
use alef_core::{
    protocol::Limits,
    registry::{command::Reply, context::CallContext, dispatch::Registry},
    session::{executor::BlockingExecutor, SessionManager, TokenSource},
};
use std::{
    future::Future,
    num::NonZeroUsize,
    sync::{
        atomic::{AtomicUsize, Ordering},
        mpsc, Arc,
    },
    task::Poll,
    time::Duration,
};

async fn registered(
    root: &Path,
    shell: Arc<Fake>,
    opener: MenuOpener,
    limits: Limits,
) -> (Registry, CallContext) {
    registered_with_executors(
        root,
        shell,
        opener,
        limits,
        Arc::new(BlockingExecutor::new(NonZeroUsize::new(64).unwrap())),
        Arc::new(BlockingExecutor::new(NonZeroUsize::new(64).unwrap())),
    )
    .await
}

async fn registered_with_executors(
    root: &Path,
    shell: Arc<Fake>,
    opener: MenuOpener,
    limits: Limits,
    permission_executor: Arc<BlockingExecutor>,
    work_executor: Arc<BlockingExecutor>,
) -> (Registry, CallContext) {
    let mut registry = Registry::default();
    register_with(
        &mut registry,
        shell,
        opener,
        permission_executor,
        work_executor,
    )
    .unwrap();
    let manager = SessionManager::new(
        Arc::new(|| "registry-test-session".into()) as TokenSource,
        limits,
    );
    let session = tokio::time::timeout(Duration::from_secs(10), manager.begin_document(1))
        .await
        .unwrap();
    let permissions = permissions(root, true, true, true);
    (registry, CallContext::new(session, permissions))
}

async fn dispatch(
    registry: &Registry,
    ctx: &CallContext,
    command: &str,
    args: Value,
) -> Result<Reply, AlefError> {
    tokio::time::timeout(
        Duration::from_secs(10),
        registry.dispatch(command, ctx.clone(), args),
    )
    .await
    .expect("registered command timed out")
}

fn fallback_opener() -> MenuOpener {
    Arc::new(|_, shell, _| {
        Ok((
            fallback_items_with(shell.as_ref(), &|_| Vec::new(), &[PathBuf::from("x.txt")]),
            Some(Backend::Fallback(shell)),
        ))
    })
}

fn response_handle(reply: Reply) -> u64 {
    match reply {
        Reply::Json(value) => value["handle"].as_u64().expect("menu handle"),
        _ => panic!("expected JSON response"),
    }
}

#[tokio::test]
async fn registry_dispatch_rejects_full_permission_and_work_executors_before_opening() {
    for permission_full in [true, false] {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("note.txt");
        std::fs::write(&file, b"").unwrap();
        let permission_executor = Arc::new(BlockingExecutor::new(NonZeroUsize::new(1).unwrap()));
        let work_executor = Arc::new(BlockingExecutor::new(NonZeroUsize::new(1).unwrap()));
        let saturated = if permission_full {
            &permission_executor
        } else {
            &work_executor
        };
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = mpsc::sync_channel::<()>(1);
        let blocker = saturated
            .submit(move || {
                let _ = started_tx.send(());
                let _ = release_rx.recv_timeout(Duration::from_secs(10));
            })
            .unwrap();
        tokio::time::timeout(Duration::from_secs(10), started_rx)
            .await
            .unwrap()
            .unwrap();
        let shell = Arc::new(Fake::default());
        let opened = Arc::new(AtomicUsize::new(0));
        let count = opened.clone();
        let opener: MenuOpener = Arc::new(move |_, _, _| {
            count.fetch_add(1, Ordering::SeqCst);
            Ok((Vec::new(), None))
        });
        let (registry, ctx) = registered_with_executors(
            root.path(),
            shell,
            opener,
            Limits::default(),
            permission_executor,
            work_executor,
        )
        .await;
        let err = dispatch(
            &registry,
            &ctx,
            "shell.contextMenu",
            json!({"paths":[file]}),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::Busy);
        assert_eq!(ctx.resources().len(), 0);
        assert_eq!(opened.load(Ordering::SeqCst), 0);
        if !permission_full {
            let undeclared = CallContext::new(
                ctx.session.clone(),
                permissions(root.path(), false, true, true),
            );
            let err = dispatch(
                &registry,
                &undeclared,
                "shell.contextMenu",
                json!({"paths":[file]}),
            )
            .await
            .unwrap_err();
            assert_eq!(err.code, ErrorCode::PermissionDenied);
            assert_eq!(undeclared.resources().len(), 0);
            assert_eq!(opened.load(Ordering::SeqCst), 0);
        }
        release_tx.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(10), blocker.result())
            .await
            .unwrap()
            .unwrap();
        let handle = response_handle(
            dispatch(
                &registry,
                &ctx,
                "shell.contextMenu",
                json!({"paths":[file]}),
            )
            .await
            .unwrap(),
        );
        assert_eq!(opened.load(Ordering::SeqCst), 1);
        dispatch(
            &registry,
            &ctx,
            "shell.contextMenu.close",
            json!({"handle":handle}),
        )
        .await
        .unwrap();
    }
}

#[test]
fn queued_invoke_observes_cancellation_before_side_effect() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .unwrap();
    runtime.block_on(async {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("note.txt");
        std::fs::write(&file, b"").unwrap();
        let work = Arc::new(BlockingExecutor::new(NonZeroUsize::new(4).unwrap()));
        let shell = Arc::new(Fake::default());
        let (registry, ctx) = registered_with_executors(
            root.path(),
            shell.clone(),
            fallback_opener(),
            Limits::default(),
            Arc::new(BlockingExecutor::new(NonZeroUsize::new(4).unwrap())),
            work.clone(),
        )
        .await;
        let handle = response_handle(
            dispatch(
                &registry,
                &ctx,
                "shell.contextMenu",
                json!({"paths":[file]}),
            )
            .await
            .unwrap(),
        );
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = mpsc::sync_channel::<()>(1);
        let blocker = work
            .submit(move || {
                let _ = started_tx.send(());
                let _ = release_rx.recv_timeout(Duration::from_secs(10));
            })
            .unwrap();
        tokio::time::timeout(Duration::from_secs(10), started_rx)
            .await
            .unwrap()
            .unwrap();
        let (sender, cancel) = alef_core::registry::context::CancelHandle::channel();
        let invoke = registry.dispatch(
            "shell.contextMenu.invoke",
            ctx.clone().with_cancel(cancel),
            json!({"handle":handle,"id":"reveal"}),
        );
        tokio::pin!(invoke);
        std::future::poll_fn(|cx| {
            assert!(matches!(invoke.as_mut().poll(cx), Poll::Pending));
            Poll::Ready(())
        })
        .await;
        sender.cancel();
        release_tx.send(()).unwrap();
        let err = tokio::time::timeout(Duration::from_secs(10), invoke)
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::Closed);
        tokio::time::timeout(Duration::from_secs(10), blocker.result())
            .await
            .unwrap()
            .unwrap();
        assert!(shell.calls.lock().unwrap().is_empty());
        dispatch(
            &registry,
            &ctx,
            "shell.contextMenu.close",
            json!({"handle":handle}),
        )
        .await
        .unwrap();
    });
}

#[test]
fn queued_invoke_observes_session_close_before_side_effect() {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .max_blocking_threads(1)
        .build()
        .unwrap();
    runtime.block_on(async {
        let root = tempfile::tempdir().unwrap();
        let file = root.path().join("note.txt");
        std::fs::write(&file, b"").unwrap();
        let work = Arc::new(BlockingExecutor::new(NonZeroUsize::new(4).unwrap()));
        let shell = Arc::new(Fake::default());
        let (registry, ctx) = registered_with_executors(
            root.path(),
            shell.clone(),
            fallback_opener(),
            Limits::default(),
            Arc::new(BlockingExecutor::new(NonZeroUsize::new(4).unwrap())),
            work.clone(),
        )
        .await;
        let handle = response_handle(
            dispatch(
                &registry,
                &ctx,
                "shell.contextMenu",
                json!({"paths":[file]}),
            )
            .await
            .unwrap(),
        );
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = mpsc::sync_channel::<()>(1);
        let blocker = work
            .submit(move || {
                let _ = started_tx.send(());
                let _ = release_rx.recv_timeout(Duration::from_secs(10));
            })
            .unwrap();
        tokio::time::timeout(Duration::from_secs(10), started_rx)
            .await
            .unwrap()
            .unwrap();
        let invoke = registry.dispatch(
            "shell.contextMenu.invoke",
            ctx.clone(),
            json!({"handle":handle,"id":"reveal"}),
        );
        tokio::pin!(invoke);
        std::future::poll_fn(|cx| {
            assert!(matches!(invoke.as_mut().poll(cx), Poll::Pending));
            Poll::Ready(())
        })
        .await;
        let closing = ctx.session.close();
        tokio::pin!(closing);
        std::future::poll_fn(|cx| {
            let _ = closing.as_mut().poll(cx);
            assert!(!ctx.session.is_open());
            Poll::Ready(())
        })
        .await;
        release_tx.send(()).unwrap();
        let err = tokio::time::timeout(Duration::from_secs(10), invoke)
            .await
            .unwrap()
            .unwrap_err();
        assert_eq!(err.code, ErrorCode::Closed);
        tokio::time::timeout(Duration::from_secs(10), closing)
            .await
            .unwrap();
        tokio::time::timeout(Duration::from_secs(10), blocker.result())
            .await
            .unwrap()
            .unwrap();
        assert!(shell.calls.lock().unwrap().is_empty());
    });
}
#[tokio::test]
async fn registered_mixed_parent_selection_fails_before_opener_and_resource_allocation() {
    let root = tempfile::tempdir().unwrap();
    let a = root.path().join("a.txt");
    let sub = root.path().join("sub");
    std::fs::create_dir(&sub).unwrap();
    let b = sub.join("b.txt");
    std::fs::write(&a, b"").unwrap();
    std::fs::write(&b, b"").unwrap();
    let shell = Arc::new(Fake::default());
    let opened = Arc::new(AtomicUsize::new(0));
    let count = opened.clone();
    let opener: MenuOpener = Arc::new(move |_, _, _| {
        count.fetch_add(1, Ordering::SeqCst);
        Ok((Vec::new(), None))
    });
    let (registry, ctx) = registered(root.path(), shell, opener, Limits::default()).await;
    let ctx = CallContext::new(
        ctx.session.clone(),
        permissions(root.path(), true, true, true),
    );
    let err = dispatch(&registry, &ctx, "shell.contextMenu", json!({"paths":[a,b]}))
        .await
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::InvalidArgument);
    assert_eq!(opened.load(Ordering::SeqCst), 0);
    assert_eq!(ctx.resources().len(), 0);
}

#[tokio::test]
async fn registered_creation_reserves_before_opener_and_releases_failed_reservation() {
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("note.txt");
    std::fs::write(&file, b"").unwrap();
    let shell = Arc::new(Fake::default());
    let resources = Arc::new(Mutex::new(
        None::<Arc<alef_core::registry::context::CallContext>>,
    ));
    let opener_calls = Arc::new(AtomicUsize::new(0));
    let calls = opener_calls.clone();
    let captured = resources.clone();
    let opener: MenuOpener = Arc::new(move |_, _, _| {
        calls.fetch_add(1, Ordering::SeqCst);
        let ctx = captured.lock().unwrap().as_ref().unwrap().clone();
        assert_eq!(ctx.resources().len(), 1, "resource must precede opener");
        Err(AlefError::new(ErrorCode::Internal, "opener sentinel"))
    });
    let (registry, ctx) = registered(root.path(), shell, opener, Limits::default()).await;
    let ctx = CallContext::new(
        ctx.session.clone(),
        permissions(root.path(), true, true, true),
    );
    *resources.lock().unwrap() = Some(Arc::new(ctx.clone()));
    for _ in 0..2 {
        let err = dispatch(
            &registry,
            &ctx,
            "shell.contextMenu",
            json!({"paths":[file]}),
        )
        .await
        .unwrap_err();
        assert_eq!(err.code, ErrorCode::Internal);
        assert_eq!(err.message, "opener sentinel");
        assert_eq!(ctx.resources().len(), 0);
    }
    assert_eq!(opener_calls.load(Ordering::SeqCst), 2);
}

#[tokio::test]
async fn registered_menu_obeys_session_resource_limit_and_close_releases_capacity() {
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("note.txt");
    std::fs::write(&file, b"").unwrap();
    let mut limits = Limits::default();
    limits.max_resources_per_session = 1;
    let opened = Arc::new(AtomicUsize::new(0));
    let count = opened.clone();
    let opener: MenuOpener = Arc::new(move |paths, shell, _| {
        count.fetch_add(1, Ordering::SeqCst);
        Ok((
            fallback_items_with(shell.as_ref(), &|_| Vec::new(), paths),
            Some(Backend::Fallback(shell)),
        ))
    });
    let (registry, initial) =
        registered(root.path(), Arc::new(Fake::default()), opener, limits).await;
    let ctx = CallContext::new(
        initial.session.clone(),
        permissions(root.path(), true, true, true),
    );
    let first = response_handle(
        dispatch(
            &registry,
            &ctx,
            "shell.contextMenu",
            json!({"paths":[file]}),
        )
        .await
        .unwrap(),
    );
    let err = dispatch(
        &registry,
        &ctx,
        "shell.contextMenu",
        json!({"paths":[file]}),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, ErrorCode::Busy);
    assert_eq!(opened.load(Ordering::SeqCst), 1);
    dispatch(
        &registry,
        &ctx,
        "shell.contextMenu.close",
        json!({"handle":first}),
    )
    .await
    .unwrap();
    let second = response_handle(
        dispatch(
            &registry,
            &ctx,
            "shell.contextMenu",
            json!({"paths":[file]}),
        )
        .await
        .unwrap(),
    );
    assert_eq!(opened.load(Ordering::SeqCst), 2);
    dispatch(
        &registry,
        &ctx,
        "shell.contextMenu.close",
        json!({"handle":second}),
    )
    .await
    .unwrap();
}

#[tokio::test]
async fn registered_invoke_rejects_bat_open_but_allows_reveal_and_close_removes_handle() {
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("script.bat");
    std::fs::write(&file, b"").unwrap();
    let shell = Arc::new(Fake::default());
    let (registry, initial) = registered(
        root.path(),
        shell.clone(),
        fallback_opener(),
        Limits::default(),
    )
    .await;
    let ctx = CallContext::new(
        initial.session.clone(),
        permissions(root.path(), true, true, true),
    );
    let handle = response_handle(
        dispatch(
            &registry,
            &ctx,
            "shell.contextMenu",
            json!({"paths":[file]}),
        )
        .await
        .unwrap(),
    );
    let err = dispatch(
        &registry,
        &ctx,
        "shell.contextMenu.invoke",
        json!({"handle":handle,"id":"open"}),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, ErrorCode::PermissionDenied);
    assert_eq!(err.message, "shell.openPath does not start programs");
    assert!(shell.calls.lock().unwrap().is_empty());
    dispatch(
        &registry,
        &ctx,
        "shell.contextMenu.invoke",
        json!({"handle":handle,"id":"reveal"}),
    )
    .await
    .unwrap();
    assert_eq!(shell.calls.lock().unwrap().len(), 1);
    dispatch(
        &registry,
        &ctx,
        "shell.contextMenu.close",
        json!({"handle":handle}),
    )
    .await
    .unwrap();
    let err = dispatch(
        &registry,
        &ctx,
        "shell.contextMenu.invoke",
        json!({"handle":handle,"id":"reveal"}),
    )
    .await
    .unwrap_err();
    assert_eq!(err.code, ErrorCode::NotFound);
}
