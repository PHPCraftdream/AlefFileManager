// SPDX-License-Identifier: MIT OR Apache-2.0
use super::*;

#[derive(Debug, Default)]
pub(super) struct Fake {
    pub(super) calls: Mutex<Vec<String>>,
}
impl ShellBackend for Fake {
    fn open_external(&self, _: &str) -> Result<(), AlefError> {
        Ok(())
    }
    fn open_path(&self, path: &Path) -> Result<(), AlefError> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("open:{}", path.display()));
        Ok(())
    }
    fn show_in_folder(&self, path: &Path) -> Result<(), AlefError> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("reveal:{}", path.display()));
        Ok(())
    }
    fn trash(&self, path: &Path) -> Result<(), AlefError> {
        self.calls
            .lock()
            .unwrap()
            .push(format!("trash:{}", path.display()));
        Ok(())
    }
}
fn state(
    paths: Vec<PathBuf>,
    originals: Vec<String>,
    items: Vec<ContextMenuItem>,
    shell: Arc<Fake>,
) -> Arc<Mutex<State>> {
    Arc::new(Mutex::new(State {
        closed: false,
        substituted: false,
        paths,
        originals,
        items,
        backend: Some(Backend::Fallback(shell)),
    }))
}
pub(super) fn permissions(
    root: &Path,
    menu: bool,
    read: bool,
    write: bool,
) -> Arc<alef_core::security::permissions::PermissionSet> {
    use alef_core::security::{
        manifest::Permissions,
        permissions::{PathVars, PermissionSet},
    };
    let vars = PathVars {
        app_data: root.join("data"),
        app_config: root.join("config"),
        app_cache: root.join("cache"),
        home: root.join("home"),
        documents: root.to_owned(),
        downloads: root.to_owned(),
        desktop: root.to_owned(),
        temp: root.to_owned(),
        app: Some(root.to_owned()),
    };
    let mut value = serde_json::json!({"fs":{"read":[],"write":[]},"cli":{"exec":[]},"net":{"http":[],"socket":[]},"shell":{"openExternal":[],"contextMenu":menu},"clipboard":{"read":false},"shortcut":{"global":false},"secrets":false,"app":{"env":[]}});
    value["fs"]["read"] = if read {
        serde_json::json!([format!("{}/**", root.display())])
    } else {
        serde_json::json!([])
    };
    value["fs"]["write"] = if write {
        serde_json::json!([format!("{}/**", root.display())])
    } else {
        serde_json::json!([])
    };
    let manifest: Permissions = serde_json::from_value(value).unwrap();
    Arc::new(PermissionSet::from_manifest(&manifest, &vars).unwrap())
}
fn run(
    state: Arc<Mutex<State>>,
    id: &str,
    perms: Arc<alef_core::security::permissions::PermissionSet>,
) -> Result<(), AlefError> {
    invoke_state(
        state,
        id.into(),
        perms,
        Arc::new(alef_core::security::permissions::Grants::new()),
        || true,
    )
}
fn item_set() -> Vec<ContextMenuItem> {
    fallback_items_with(
        &Fake::default(),
        &|_| vec![("editor".into(), "Editor".into())],
        &[PathBuf::from("x.txt")],
    )
}

#[tokio::test]
async fn authorization_canonicalizes_and_rejects_empty_or_mixed_parent_selections() {
    use alef_core::security::consent::{Decision, Right};
    use std::time::Duration;
    let root = tempfile::tempdir().unwrap();
    let f = root.path().join("a.txt");
    std::fs::write(&f, b"").unwrap();
    let p = permissions(root.path(), true, true, false);
    let tokens: alef_core::session::session::TokenSource = Arc::new(|| "test-token".into());
    let manager =
        alef_core::session::SessionManager::new(tokens, alef_core::protocol::Limits::default());
    let session = tokio::time::timeout(Duration::from_secs(10), manager.begin_document(1))
        .await
        .unwrap();
    let ctx = alef_core::registry::context::CallContext::new(session, p.clone());
    let (paths, sub) = authorized(&ctx, &[f.to_string_lossy().into_owned()]).unwrap();
    assert!(!sub);
    assert!(paths[0].is_absolute());
    let substituted = p.as_ref().clone().with_consent({
        let mut consent = alef_core::security::consent::Consent::allow_all();
        consent.set(Right::plain("shell.contextMenu"), Decision::Substitute);
        consent
    });
    let substituted_ctx =
        alef_core::registry::context::CallContext::new(ctx.session.clone(), Arc::new(substituted));
    assert!(
        authorized(&substituted_ctx, &[f.to_string_lossy().into_owned()])
            .unwrap()
            .1
    );
    let undeclared = permissions(root.path(), false, true, false);
    let undeclared_ctx =
        alef_core::registry::context::CallContext::new(ctx.session.clone(), undeclared);
    assert_eq!(
        authorized(&undeclared_ctx, &[f.to_string_lossy().into_owned()])
            .unwrap_err()
            .code,
        ErrorCode::PermissionDenied
    );
    let mut consent = alef_core::security::consent::Consent::allow_all();
    consent.set(
        Right::scoped("fs.read", &format!("{}/**", root.path().display())),
        Decision::Substitute,
    );
    let read_substituted = Arc::new(p.as_ref().clone().with_consent(consent));
    let read_substituted_ctx =
        alef_core::registry::context::CallContext::new(ctx.session.clone(), read_substituted);
    assert!(
        authorized(&read_substituted_ctx, &[f.to_string_lossy().into_owned()])
            .unwrap()
            .1
    );
    assert_eq!(
        authorized(&ctx, &[]).unwrap_err().code,
        ErrorCode::InvalidArgument
    );
    assert_eq!(
        rules::same_parent(&[f.clone(), root.path().join("sub/b.txt")])
            .unwrap_err()
            .code,
        ErrorCode::InvalidArgument
    );
}

#[tokio::test]
async fn runas_is_denied_before_item_validation_or_backend_invocation() {
    let root = tempfile::tempdir().unwrap();
    let file = root.path().join("note.txt");
    std::fs::write(&file, b"").unwrap();
    let fake = Arc::new(Fake::default());
    let menu = state(
        vec![file.clone()],
        vec![file.to_string_lossy().into_owned()],
        item_set(),
        fake.clone(),
    );
    let error = run(menu, "runas", permissions(root.path(), true, true, false)).unwrap_err();
    assert_eq!(error.code, ErrorCode::PermissionDenied);
    assert!(fake.calls.lock().unwrap().is_empty());
}

#[tokio::test]
async fn invocation_refuses_dangerous_open_before_any_backend_call_and_allows_reveal() {
    let root = tempfile::tempdir().unwrap();
    let txt = root.path().join("note.txt");
    let bat = root.path().join("script.bat");
    std::fs::write(&txt, b"").unwrap();
    std::fs::write(&bat, b"").unwrap();
    // Menu state holds authorized (resolved) paths; macOS temp lives behind /var -> /private/var.
    #[cfg(unix)]
    let (txt, bat) = (txt.canonicalize().unwrap(), bat.canonicalize().unwrap());
    let fake = Arc::new(Fake::default());
    let p = permissions(txt.parent().unwrap(), true, true, true);
    let menu = state(
        vec![bat.clone()],
        vec![bat.to_string_lossy().into_owned()],
        item_set(),
        fake.clone(),
    );
    let err = run(menu.clone(), "open", p.clone()).unwrap_err();
    assert_eq!(err.code, ErrorCode::PermissionDenied);
    assert_eq!(err.message, "shell.openPath does not start programs");
    assert!(fake.calls.lock().unwrap().is_empty());
    let mixed = state(
        vec![txt.clone(), bat.clone()],
        vec![
            txt.to_string_lossy().into_owned(),
            bat.to_string_lossy().into_owned(),
        ],
        item_set(),
        fake.clone(),
    );
    assert_eq!(
        run(mixed, "open", p.clone()).unwrap_err().code,
        ErrorCode::PermissionDenied
    );
    assert!(fake.calls.lock().unwrap().is_empty());
    run(menu, "reveal", p).unwrap();
    assert_eq!(fake.calls.lock().unwrap().len(), 1);
    let menu = state(
        vec![txt.clone()],
        vec![txt.to_string_lossy().into_owned()],
        item_set(),
        fake.clone(),
    );
    run(menu, "open", permissions(root.path(), true, true, true)).unwrap();
    assert_eq!(fake.calls.lock().unwrap().len(), 2);
    let trash = state(
        vec![txt.clone()],
        vec![txt.to_string_lossy().into_owned()],
        item_set(),
        fake.clone(),
    );
    assert_eq!(
        run(trash, "trash", permissions(root.path(), true, true, false))
            .unwrap_err()
            .code,
        ErrorCode::PermissionDenied
    );
    assert_eq!(fake.calls.lock().unwrap().len(), 2);
}

#[tokio::test]
async fn table_reserves_capacity_and_close_disables_menu_state() {
    use std::time::Duration;
    let table = alef_core::session::ResourceTable::new(1);
    let fake = Arc::new(Fake::default());
    let menu = MenuResource(state(vec![], vec![], item_set(), fake.clone()));
    let handle = table.insert(Box::new(menu.clone())).unwrap();
    assert_eq!(
        table.insert(Box::new(menu.clone())).unwrap_err().code,
        ErrorCode::Busy
    );
    table.remove(handle).unwrap();
    assert!(table.insert(Box::new(menu.clone())).is_ok());
    let manager = alef_core::session::SessionManager::new(
        Arc::new(|| "test-token".into()),
        alef_core::protocol::Limits::default(),
    );
    let session = tokio::time::timeout(Duration::from_secs(10), manager.begin_document(1))
        .await
        .unwrap();
    let resource = MenuResource(state(vec![], vec![], item_set(), fake.clone()));
    let resource_handle = session
        .resources()
        .insert(Box::new(resource.clone()))
        .unwrap();
    tokio::time::timeout(Duration::from_secs(10), manager.close_all())
        .await
        .unwrap();
    assert!(!session.is_open());
    assert!(resource.0.lock().unwrap().closed);
    assert!(resource.0.lock().unwrap().backend.is_none());
    assert_eq!(
        session
            .resources()
            .with_as::<MenuResource, _>(resource_handle, |_| ())
            .unwrap_err()
            .code,
        ErrorCode::NotFound
    );
    menu.close_now();
    assert!(menu.0.lock().unwrap().closed);
    assert!(menu.0.lock().unwrap().backend.is_none());
    let root = tempfile::tempdir().unwrap();
    assert_eq!(
        run(
            menu.0.clone(),
            "open",
            permissions(root.path(), true, true, false)
        )
        .unwrap_err()
        .code,
        ErrorCode::NotFound
    );
}

#[test]
fn inactive_invocation_is_closed_before_permission_checks() {
    let root = tempfile::tempdir().unwrap();
    let fake = Arc::new(Fake::default());
    let state = state(vec![], vec![], item_set(), fake.clone());
    let error = invoke_state(
        state,
        "open".into(),
        permissions(root.path(), false, false, false),
        Arc::new(alef_core::security::permissions::Grants::new()),
        || false,
    )
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::Closed);
    assert!(fake.calls.lock().unwrap().is_empty());
}
