// SPDX-License-Identifier: MIT OR Apache-2.0
//! Tests of the runtime authorization: rights, decisions, narrowing and refusal.
use super::*;
use std::num::NonZeroUsize;
use std::sync::mpsc;
use std::time::Duration;

const LIMIT: Duration = Duration::from_secs(5);

fn executor(capacity: usize) -> crate::session::BlockingExecutor {
    crate::session::BlockingExecutor::new(NonZeroUsize::new(capacity).expect("non-zero test limit"))
}

fn set() -> Arc<PermissionSet> {
    // Minimal manifest: the empty fs scopes make the grants the only source of access.
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
    let root = std::env::temp_dir();
    let vars = PathVars {
        app_data: root.clone(),
        app_config: root.clone(),
        app_cache: root.clone(),
        home: root.clone(),
        documents: root.clone(),
        downloads: root.clone(),
        desktop: root.clone(),
        temp: root.clone(),
        app: Some(root),
    };
    Arc::new(PermissionSet::from_manifest(&policy, &vars).expect("permission set"))
}

/// A read grant for `dir` and a file inside it; the grant is shared as one `Arc`.
fn granted(dir: &Path) -> (Arc<Grants>, PathBuf) {
    let file = dir.join("file.txt");
    std::fs::write(&file, b"").expect("file created");
    let grants = Arc::new(Grants::new());
    grants.grant_read(dir).expect("grant read");
    (grants, file)
}

#[tokio::test]
async fn a_permission_decision_is_the_same_on_and_off_the_executor() {
    let set = set();
    let dir = tempfile::tempdir().expect("tempdir");
    let (grants, file) = granted(dir.path());
    let executor = executor(2);

    // (a) a granted path is allowed on both routes
    let target = file.to_str().expect("utf-8 path");
    let inline = set
        .check(Permission::FsRead, Some(target), &grants)
        .expect("inline check");
    let on_executor = tokio::time::timeout(
        LIMIT,
        set.clone()
            .check_on(&executor, Permission::FsRead, Some(target), &grants),
    )
    .await
    .expect("check_on completes")
    .expect("on-executor check");
    assert_eq!(inline, Decision::Allow);
    assert_eq!(on_executor, Decision::Allow);

    // (b) a path outside the grant is refused the same way
    let outside = std::env::temp_dir().join("alef-permissions-outside.txt");
    let outside = outside.to_str().expect("utf-8 path").to_owned();
    assert_eq!(
        set.check(Permission::FsRead, Some(&outside), &grants)
            .unwrap_err()
            .code,
        ErrorCode::PermissionDenied
    );
    assert_eq!(
        tokio::time::timeout(
            LIMIT,
            set.clone()
                .check_on(&executor, Permission::FsRead, Some(&outside), &grants,),
        )
        .await
        .expect("check_on completes")
        .unwrap_err()
        .code,
        ErrorCode::PermissionDenied
    );

    // (c) no capability is allowed without touching the executor
    assert_eq!(
        set.check(Permission::None, None, &grants)
            .expect("no capability"),
        Decision::Allow
    );
    assert_eq!(
        tokio::time::timeout(
            LIMIT,
            set.check_on(&executor, Permission::None, None, &grants)
        )
        .await
        .expect("check_on completes")
        .expect("no capability"),
        Decision::Allow
    );
}

#[tokio::test]
async fn a_path_authorization_is_the_same_on_and_off_the_executor() {
    let set = set();
    let dir = tempfile::tempdir().expect("tempdir");
    let (grants, file) = granted(dir.path());
    let executor = executor(2);
    let target = file.to_str().expect("utf-8 path");

    let inline = set
        .authorize_at(Permission::FsRead, Some(target), &grants, Reach::Through)
        .expect("inline authorization");
    let on_executor = tokio::time::timeout(
        LIMIT,
        set.clone().authorize_at_on(
            &executor,
            Permission::FsRead,
            Some(target),
            &grants,
            Reach::Through,
        ),
    )
    .await
    .expect("authorize_at_on completes")
    .expect("on-executor authorization");
    assert_eq!(inline, on_executor);
    assert_eq!(
        Some(&inline.path),
        canonical(Path::new(target)).as_ref(),
        "the authorized path is the canonical one"
    );
}

#[tokio::test]
async fn a_saturated_permission_executor_refuses_the_check_with_busy() {
    let set = set();
    let dir = tempfile::tempdir().expect("tempdir");
    let (grants, file) = granted(dir.path());
    let executor = executor(1);
    let target = file.to_str().expect("utf-8 path").to_owned();

    let (started_tx, started_rx) = tokio::sync::oneshot::channel();
    let (release_tx, release_rx) = mpsc::sync_channel::<()>(1);
    let blocker = executor
        .submit(move || {
            let _ = started_tx.send(());
            let _ = release_rx.recv_timeout(LIMIT);
        })
        .expect("blocker admitted");
    tokio::time::timeout(LIMIT, started_rx)
        .await
        .expect("blocker started")
        .expect("signal");

    assert_eq!(
        tokio::time::timeout(
            LIMIT,
            set.check_on(&executor, Permission::FsRead, Some(&target), &grants)
        )
        .await
        .expect("check_on completes")
        .unwrap_err()
        .code,
        ErrorCode::Busy
    );
    release_tx.send(()).expect("blocker waiting");
    tokio::time::timeout(LIMIT, blocker.result())
        .await
        .expect("blocker exits")
        .expect("blocker joins");
}

fn policy(media: Option<crate::security::manifest::MediaPermissions>, audio: bool) -> Permissions {
    serde_json::from_value(json!({
        "fs": {"read": [], "write": []},
        "cli": {"exec": []},
        "net": {"http": [], "socket": []},
        "shell": {"openExternal": []},
        "clipboard": {"read": false},
        "shortcut": {"global": false},
        "secrets": false,
        "media": media,
        "audio": audio,
        "app": {"env": []},
    }))
    .expect("policy")
}

#[test]
fn media_rights_are_enumerated_only_when_the_manifest_declares_them() {
    let root = std::env::temp_dir();
    let vars = PathVars {
        app_data: root.clone(),
        app_config: root.clone(),
        app_cache: root.clone(),
        home: root.clone(),
        documents: root.clone(),
        downloads: root.clone(),
        desktop: root.clone(),
        temp: root.clone(),
        app: Some(root),
    };
    let shut = PermissionSet::from_manifest(&policy(None, false), &vars).expect("set");
    let names: Vec<String> = shut.rights().iter().map(|r| r.permission.clone()).collect();
    assert!(!names.contains(&"media.camera".to_owned()));
    assert!(!names.contains(&"media.microphone".to_owned()));
    assert!(!names.contains(&"audio".to_owned()));
    let open = PermissionSet::from_manifest(
        &policy(
            Some(crate::security::manifest::MediaPermissions {
                camera: true,
                microphone: true,
            }),
            true,
        ),
        &vars,
    )
    .expect("set");
    let rights = open.rights();
    assert!(rights.contains(&Right::plain("media.camera")));
    assert!(rights.contains(&Right::plain("media.microphone")));
    assert!(rights.contains(&Right::plain("audio")));
}

#[test]
fn media_rights_decide_allow_substitute_and_deny_like_the_other_plain_rights() {
    let root = std::env::temp_dir();
    let vars = PathVars {
        app_data: root.clone(),
        app_config: root.clone(),
        app_cache: root.clone(),
        home: root.clone(),
        documents: root.clone(),
        downloads: root.clone(),
        desktop: root.clone(),
        temp: root.clone(),
        app: Some(root),
    };
    let media = Some(crate::security::manifest::MediaPermissions {
        camera: true,
        microphone: true,
    });
    let set = PermissionSet::from_manifest(&policy(media.clone(), true), &vars)
        .expect("set")
        .with_consent(Consent::allow_all());
    let grants = Grants::new();
    for permission in [
        Permission::MediaCamera,
        Permission::MediaMicrophone,
        Permission::Audio,
    ] {
        assert_eq!(
            set.check(permission, None, &grants).expect("allowed"),
            Decision::Allow
        );
    }
    let shut = PermissionSet::from_manifest(&policy(media.clone(), true), &vars)
        .expect("set")
        .with_consent(Consent::undecided());
    for permission in [
        Permission::MediaCamera,
        Permission::MediaMicrophone,
        Permission::Audio,
    ] {
        let err = shut.check(permission, None, &grants).unwrap_err();
        assert_eq!(err.code, ErrorCode::PermissionDenied);
        assert_eq!(err.details, Some(json!({"permission": permission.name()})));
    }
    let mut replaced = Consent::undecided();
    replaced.set(Right::plain("media.camera"), Decision::Substitute);
    let replaced = PermissionSet::from_manifest(&policy(media.clone(), true), &vars)
        .expect("set")
        .with_consent(replaced);
    assert_eq!(
        replaced
            .check(Permission::MediaCamera, None, &grants)
            .expect("substituted"),
        Decision::Substitute
    );
}

#[test]
fn narrowing_applies_to_media_rights() {
    let root = std::env::temp_dir();
    let vars = PathVars {
        app_data: root.clone(),
        app_config: root.clone(),
        app_cache: root.clone(),
        home: root.clone(),
        documents: root.clone(),
        downloads: root.clone(),
        desktop: root.clone(),
        temp: root.clone(),
        app: Some(root),
    };
    let media = Some(crate::security::manifest::MediaPermissions {
        camera: true,
        microphone: true,
    });
    let set = PermissionSet::from_manifest(&policy(media.clone(), true), &vars)
        .expect("set")
        .with_consent(Consent::allow_all());
    let mut stored = Consent::undecided();
    stored.set(Right::plain("media.camera"), Decision::Deny);
    assert!(set.narrow(&stored));
    let grants = Grants::new();
    let err = set
        .check(Permission::MediaCamera, None, &grants)
        .unwrap_err();
    assert_eq!(err.code, ErrorCode::PermissionDenied);
    assert_eq!(
        set.check(Permission::Audio, None, &grants)
            .expect("allowed"),
        Decision::Allow
    );
}
