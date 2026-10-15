// SPDX-License-Identifier: MIT OR Apache-2.0
use super::tests::{permissions, Fake};
use super::*;
use alef_core::{
    protocol::Limits,
    registry::{command::Reply, context::CallContext, dispatch::Registry},
    session::{executor::BlockingExecutor, SessionManager, TokenSource},
};
use std::{
    num::NonZeroUsize,
    sync::atomic::{AtomicUsize, Ordering},
    time::Duration,
};

async fn registered(
    root: &Path,
    shell: Arc<Fake>,
    opener: MenuOpener,
    limits: Limits,
) -> (Registry, CallContext) {
    let mut registry = Registry::default();
    let executor = Arc::new(BlockingExecutor::new(NonZeroUsize::new(64).unwrap()));
    register_with(&mut registry, shell, opener, executor.clone(), executor).unwrap();
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
