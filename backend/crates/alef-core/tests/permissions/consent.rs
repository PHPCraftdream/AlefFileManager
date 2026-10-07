// SPDX-License-Identifier: MIT OR Apache-2.0
//! The decisions of the user: the manifest says what may be asked, the decision what is given.
use std::{fs, path::Path, sync::Arc};

use alef_core::{
    protocol::Limits,
    registry::{command::Reply, context::CallContext, dispatch::Registry},
    security::{
        consent::{Consent, Decision, Right},
        grants::Grants,
        manifest::Permissions,
        permissions::{PathVars, Permission, PermissionSet},
    },
    session::SessionManager,
    ErrorCode,
};
use serde_json::{json, Value};

fn vars(root: &Path) -> PathVars {
    let at = |name: &str| root.join(name);
    PathVars {
        app_data: at("data"),
        app_config: at("config"),
        app_cache: at("cache"),
        home: at("home"),
        documents: at("documents"),
        downloads: at("downloads"),
        desktop: at("desktop"),
        temp: at("temp"),
        app: at("app"),
    }
}

fn policy(extra: Value) -> Permissions {
    let mut base = json!({
        "fs": {"read": [], "write": []}, "cli": {"exec": []},
        "net": {"http": [], "socket": []}, "shell": {"openExternal": []},
        "clipboard": {"read": false}, "shortcut": {"global": false},
        "secrets": false, "app": {"env": []}
    });
    for (key, value) in extra.as_object().expect("object") {
        base[key] = value.clone();
    }
    serde_json::from_value(base).expect("permissions")
}

fn set(root: &Path, extra: Value) -> PermissionSet {
    PermissionSet::from_manifest(&policy(extra), &vars(root)).expect("permission set")
}

fn consent(decisions: &[(Right, Decision)]) -> Consent {
    let mut consent = Consent::undecided();
    for (right, decision) in decisions {
        consent.set(right.clone(), *decision);
    }
    consent
}

fn check(
    set: &PermissionSet,
    permission: Permission,
    target: Option<&str>,
) -> Result<Decision, alef_core::AlefError> {
    set.check(permission, target, &Grants::new())
}

fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

#[test]
fn the_rights_of_a_manifest_are_listed_once_each_in_a_stable_order() {
    let root = tempfile::tempdir().unwrap();
    let permissions = set(
        root.path(),
        json!({
            "fs": {"read": ["$DOCUMENTS/**", "$HOME/notes"], "write": ["$TEMP/**"]},
            "cli": {"exec": ["git"]},
            "net": {"http": ["https://example.com/*"], "socket": []},
            "shell": {"openExternal": ["https://example.com/docs/*"]},
            "clipboard": {"read": true}, "secrets": true,
            "app": {"env": ["PATH", "HOME", "PATH"]}
        }),
    )
    .with_window_create();
    let rights = permissions.rights();
    let named: Vec<String> = rights
        .iter()
        .map(|right| match &right.scope {
            Some(scope) => format!("{} {scope}", right.permission),
            None => right.permission.clone(),
        })
        .collect();
    assert_eq!(
        named,
        [
            "app.env HOME",
            "app.env PATH",
            "cli.exec git",
            "clipboard.read",
            "fs.read $DOCUMENTS/**",
            "fs.read $HOME/notes",
            "fs.write $TEMP/**",
            "net.http https://example.com/*",
            "secrets",
            "shell.openExternal https://example.com/docs/*",
            "window.create",
        ]
    );
    let again = set(root.path(), json!({"app": {"env": ["HOME", "PATH"]}})).rights();
    assert_eq!(
        again.len(),
        2,
        "a closed manifest asks for what it lists only"
    );
    assert!(set(root.path(), json!({})).rights().is_empty());
}

#[test]
fn without_a_user_everything_the_manifest_lists_is_given_and_nothing_else() {
    let root = tempfile::tempdir().unwrap();
    let permissions = set(
        root.path(),
        json!({"app": {"env": ["HOME"]}, "clipboard": {"read": true}}),
    );
    assert_eq!(
        check(&permissions, Permission::AppEnv, Some("HOME")).unwrap(),
        Decision::Allow
    );
    assert_eq!(
        check(&permissions, Permission::ClipboardRead, None).unwrap(),
        Decision::Allow
    );
    assert_eq!(
        check(&permissions, Permission::None, None).unwrap(),
        Decision::Allow
    );
    for (permission, target) in [
        (Permission::AppEnv, Some("SECRET")),
        (Permission::Secrets, None),
        (Permission::ShortcutGlobal, None),
        (Permission::NetHttp, Some("https://example.com/")),
    ] {
        assert_eq!(
            check(&permissions, permission, target).unwrap_err().code,
            ErrorCode::PermissionDenied,
            "{permission:?}"
        );
    }
}

#[test]
fn the_three_decisions_each_have_their_effect_on_a_right() {
    let root = tempfile::tempdir().unwrap();
    let base = set(
        root.path(),
        json!({"app": {"env": ["HOME", "PATH", "LANG"]}}),
    );
    let permissions = base.with_consent(consent(&[
        (Right::scoped("app.env", "HOME"), Decision::Allow),
        (Right::scoped("app.env", "PATH"), Decision::Substitute),
        (Right::scoped("app.env", "LANG"), Decision::Deny),
    ]));
    assert_eq!(
        check(&permissions, Permission::AppEnv, Some("HOME")).unwrap(),
        Decision::Allow
    );
    assert_eq!(
        check(&permissions, Permission::AppEnv, Some("PATH")).unwrap(),
        Decision::Substitute
    );
    let refused = check(&permissions, Permission::AppEnv, Some("LANG")).unwrap_err();
    assert_eq!(refused.code, ErrorCode::PermissionDenied);
}

#[test]
fn a_denial_of_the_user_is_the_same_error_as_a_right_the_manifest_never_listed() {
    let root = tempfile::tempdir().unwrap();
    let permissions = set(
        root.path(),
        json!({"app": {"env": ["LANG"]}, "clipboard": {"read": true}}),
    )
    .with_consent(consent(&[
        (Right::scoped("app.env", "LANG"), Decision::Deny),
        (Right::plain("clipboard.read"), Decision::Deny),
    ]));
    let by_user = check(&permissions, Permission::AppEnv, Some("LANG")).unwrap_err();
    let by_manifest = check(&permissions, Permission::AppEnv, Some("NEVER_LISTED")).unwrap_err();
    assert_eq!(
        by_user, by_manifest,
        "the application cannot tell whose decision it was"
    );
    let clipboard = check(&permissions, Permission::ClipboardRead, None).unwrap_err();
    assert_eq!(
        clipboard.details,
        Some(json!({"permission": "clipboard.read"}))
    );
}

#[test]
fn a_right_the_user_was_never_asked_about_is_denied() {
    let root = tempfile::tempdir().unwrap();
    let permissions = set(
        root.path(),
        json!({"secrets": true, "shortcut": {"global": true}}),
    )
    .with_consent(consent(&[(Right::plain("secrets"), Decision::Allow)]));
    assert_eq!(
        check(&permissions, Permission::Secrets, None).unwrap(),
        Decision::Allow
    );
    assert!(
        check(&permissions, Permission::ShortcutGlobal, None).is_err(),
        "not decided: denied"
    );
    assert_eq!(
        check(&permissions, Permission::None, None).unwrap(),
        Decision::Allow,
        "a command without a right is nobody's to decide"
    );
}

#[test]
fn every_scope_entry_is_decided_on_its_own() {
    let root = tempfile::tempdir().unwrap();
    for name in ["documents", "home"] {
        fs::create_dir_all(root.path().join(name)).unwrap();
    }
    let documents = root.path().join("documents").join("a.txt");
    let home = root.path().join("home").join("b.txt");
    let permissions = set(
        root.path(),
        json!({
            "fs": {"read": ["$DOCUMENTS/**", "$HOME/**"], "write": []},
            "net": {"http": ["https://one.example/*", "https://two.example/*"], "socket": []},
            "cli": {"exec": ["git", "ls"]}
        }),
    )
    .with_consent(consent(&[
        (Right::scoped("fs.read", "$DOCUMENTS/**"), Decision::Allow),
        (Right::scoped("fs.read", "$HOME/**"), Decision::Substitute),
        (
            Right::scoped("net.http", "https://one.example/*"),
            Decision::Allow,
        ),
        (
            Right::scoped("net.http", "https://two.example/*"),
            Decision::Deny,
        ),
        (Right::scoped("cli.exec", "git"), Decision::Substitute),
        (Right::scoped("cli.exec", "ls"), Decision::Allow),
    ]));
    assert_eq!(
        check(&permissions, Permission::FsRead, documents.to_str()).unwrap(),
        Decision::Allow
    );
    assert_eq!(
        check(&permissions, Permission::FsRead, home.to_str()).unwrap(),
        Decision::Substitute
    );
    assert!(check(
        &permissions,
        Permission::FsRead,
        Some(&text(&root.path().join("elsewhere")))
    )
    .is_err());
    assert_eq!(
        check(
            &permissions,
            Permission::NetHttp,
            Some("https://one.example/x")
        )
        .unwrap(),
        Decision::Allow
    );
    assert!(check(
        &permissions,
        Permission::NetHttp,
        Some("https://two.example/x")
    )
    .is_err());
    assert_eq!(
        check(&permissions, Permission::CliExec, Some("git")).unwrap(),
        Decision::Substitute
    );
    assert_eq!(
        check(&permissions, Permission::CliExec, Some("ls")).unwrap(),
        Decision::Allow
    );
}

#[test]
fn where_scopes_overlap_the_strictest_decision_of_those_that_match_wins() {
    let root = tempfile::tempdir().unwrap();
    let private = root.path().join("documents").join("private");
    fs::create_dir_all(&private).unwrap();
    let open = root.path().join("documents").join("open.txt");
    let secret = private.join("secret.txt");
    let permissions = set(
        root.path(),
        json!({"fs": {"read": ["$DOCUMENTS/**", "$DOCUMENTS/private/**"], "write": []}}),
    )
    .with_consent(consent(&[
        (Right::scoped("fs.read", "$DOCUMENTS/**"), Decision::Allow),
        (
            Right::scoped("fs.read", "$DOCUMENTS/private/**"),
            Decision::Deny,
        ),
    ]));
    assert_eq!(
        check(&permissions, Permission::FsRead, open.to_str()).unwrap(),
        Decision::Allow
    );
    assert!(
        check(&permissions, Permission::FsRead, secret.to_str()).is_err(),
        "the user closed the private folder inside the folder he opened"
    );
    let softer = set(
        root.path(),
        json!({"fs": {"read": ["$DOCUMENTS/**", "$DOCUMENTS/private/**"], "write": []}}),
    )
    .with_consent(consent(&[
        (Right::scoped("fs.read", "$DOCUMENTS/**"), Decision::Allow),
        (
            Right::scoped("fs.read", "$DOCUMENTS/private/**"),
            Decision::Substitute,
        ),
    ]));
    assert_eq!(
        check(&softer, Permission::FsRead, secret.to_str()).unwrap(),
        Decision::Substitute
    );
}

#[test]
fn what_the_user_picked_in_a_dialog_is_real_whatever_was_decided_about_the_scope() {
    let root = tempfile::tempdir().unwrap();
    let folder = root.path().join("documents");
    fs::create_dir_all(&folder).unwrap();
    let file = folder.join("picked.txt");
    fs::write(&file, "x").unwrap();
    let permissions = set(
        root.path(),
        json!({"fs": {"read": ["$DOCUMENTS/**"], "write": []}}),
    )
    .with_consent(consent(&[(
        Right::scoped("fs.read", "$DOCUMENTS/**"),
        Decision::Deny,
    )]));
    assert!(check(&permissions, Permission::FsRead, file.to_str()).is_err());
    let grants = Grants::new();
    grants.grant_read(&file).unwrap();
    assert_eq!(
        permissions
            .check(Permission::FsRead, file.to_str(), &grants)
            .unwrap(),
        Decision::Allow
    );
    let (path, decision) = permissions
        .authorize(Permission::FsRead, file.to_str(), &grants)
        .unwrap();
    assert_eq!(decision, Decision::Allow);
    assert_eq!(
        path,
        fs::canonicalize(&file)
            .map(|p| Path::new(p.to_string_lossy().trim_start_matches(r"\\?\")).to_owned())
            .unwrap()
    );
}

#[test]
fn the_folder_of_the_runtime_is_out_of_reach_of_every_right_and_every_grant() {
    let root = tempfile::tempdir().unwrap();
    let home = root.path().join("home");
    let runtime = home.join("runtime-data");
    fs::create_dir_all(runtime.join("consent")).unwrap();
    let decisions = runtime.join("consent").join("app@folder.json");
    fs::write(&decisions, "{}").unwrap();
    let ordinary = home.join("notes.txt");
    fs::write(&ordinary, "x").unwrap();
    let permissions = set(
        root.path(),
        json!({"fs": {"read": ["$HOME/**"], "write": ["$HOME/**"]}}),
    )
    .with_protected(&runtime);
    assert_eq!(
        check(&permissions, Permission::FsWrite, ordinary.to_str()).unwrap(),
        Decision::Allow
    );
    for permission in [Permission::FsRead, Permission::FsWrite] {
        for target in [
            &decisions,
            &runtime,
            &runtime.join("consent").join("new.json"),
        ] {
            assert_eq!(
                check(&permissions, permission, target.to_str())
                    .unwrap_err()
                    .code,
                ErrorCode::PermissionDenied,
                "{permission:?} {target:?}"
            );
        }
    }
    let grants = Grants::new();
    grants.grant_write(&runtime).unwrap();
    grants.grant_read(&decisions).unwrap();
    assert!(permissions
        .check(Permission::FsWrite, decisions.to_str(), &grants)
        .is_err());
    assert!(permissions
        .check(Permission::FsRead, decisions.to_str(), &grants)
        .is_err());
}

#[test]
fn the_user_can_take_a_right_back_while_the_application_runs_but_not_give_one_more() {
    let root = tempfile::tempdir().unwrap();
    let started = consent(&[
        (Right::scoped("app.env", "HOME"), Decision::Allow),
        (Right::scoped("app.env", "PATH"), Decision::Substitute),
        (Right::scoped("app.env", "LANG"), Decision::Deny),
        (Right::plain("clipboard.read"), Decision::Allow),
    ]);
    let permissions = set(
        root.path(),
        json!({"app": {"env": ["HOME", "PATH", "LANG"]}, "clipboard": {"read": true}}),
    )
    .with_consent(started);
    let holder = permissions.clone();
    let later = consent(&[
        (Right::scoped("app.env", "HOME"), Decision::Substitute),
        (Right::scoped("app.env", "PATH"), Decision::Allow),
        (Right::scoped("app.env", "LANG"), Decision::Allow),
        (Right::plain("clipboard.read"), Decision::Deny),
    ]);
    assert!(permissions.narrow(&later));
    for set in [&permissions, &holder] {
        assert_eq!(
            check(set, Permission::AppEnv, Some("HOME")).unwrap(),
            Decision::Substitute,
            "taken back to a stand-in"
        );
        assert_eq!(
            check(set, Permission::AppEnv, Some("PATH")).unwrap(),
            Decision::Substitute,
            "given more: not before the next start"
        );
        assert!(
            check(set, Permission::AppEnv, Some("LANG")).is_err(),
            "given more: not before the next start"
        );
        assert!(
            check(set, Permission::ClipboardRead, None).is_err(),
            "taken back altogether"
        );
    }
    assert!(!permissions.narrow(&later), "nothing left to narrow");
    let silent = Consent::undecided();
    assert!(
        !permissions.narrow(&silent),
        "a right the store says nothing about is left as it is"
    );
}

#[test]
fn window_creation_given_by_the_embedder_is_a_right_like_the_others() {
    let root = tempfile::tempdir().unwrap();
    let permissions = set(root.path(), json!({})).with_window_create();
    assert_eq!(permissions.rights(), [Right::plain("window.create")]);
    let denied =
        permissions.with_consent(consent(&[(Right::plain("window.create"), Decision::Deny)]));
    assert!(check(&denied, Permission::WindowCreate, None).is_err());
}

#[tokio::test]
async fn the_command_learns_what_the_user_gave_and_a_denial_never_reaches_it() {
    let root = tempfile::tempdir().unwrap();
    let base = set(
        root.path(),
        json!({"app": {"env": ["HOME", "PATH", "LANG"]}}),
    );
    let permissions = Arc::new(base.with_consent(consent(&[
        (Right::scoped("app.env", "HOME"), Decision::Allow),
        (Right::scoped("app.env", "PATH"), Decision::Substitute),
        (Right::scoped("app.env", "LANG"), Decision::Deny),
    ])));
    #[derive(serde::Deserialize)]
    struct Name {
        name: String,
    }
    let mut registry = Registry::default();
    registry
        .command::<Name>("probe.env")
        .unwrap()
        .permission(Permission::AppEnv, |args| Some(args.name.clone()))
        .substitutes()
        .handler(|ctx, _| async move { Ok(Reply::Json(json!(format!("{:?}", ctx.decision())))) })
        .unwrap();
    registry
        .command::<Name>("probe.real")
        .unwrap()
        .permission(Permission::AppEnv, |args| Some(args.name.clone()))
        .handler(|ctx, _| async move { Ok(Reply::Json(json!(format!("{:?}", ctx.decision())))) })
        .unwrap();
    registry
        .command::<()>("probe.open")
        .unwrap()
        .handler(|ctx, ()| async move { Ok(Reply::Json(json!(format!("{:?}", ctx.decision())))) })
        .unwrap();
    let session = SessionManager::new(Arc::new(|| "t".to_owned()), Limits::default())
        .begin_document(1)
        .await;
    let ask = |name: &'static str| {
        let ctx = CallContext::new(session.clone(), permissions.clone());
        let registry = registry.clone();
        async move {
            registry
                .dispatch("probe.env", ctx, json!({ "name": name }))
                .await
        }
    };
    let answer = |reply: Reply| match reply {
        Reply::Json(value) => value,
        other => panic!("{other:?}"),
    };
    assert_eq!(answer(ask("HOME").await.unwrap()), json!("Allow"));
    assert_eq!(answer(ask("PATH").await.unwrap()), json!("Substitute"));
    assert_eq!(
        ask("LANG").await.unwrap_err().code,
        ErrorCode::PermissionDenied
    );
    let real = |name: &'static str| {
        let ctx = CallContext::new(session.clone(), permissions.clone());
        let registry = registry.clone();
        async move {
            registry
                .dispatch("probe.real", ctx, json!({ "name": name }))
                .await
        }
    };
    assert_eq!(answer(real("HOME").await.unwrap()), json!("Allow"));
    assert_eq!(
        real("PATH").await.unwrap_err(),
        ask("LANG").await.unwrap_err(),
        "a command that has no stand-in never hands out the real thing to a user who chose one"
    );
    let open = registry
        .dispatch(
            "probe.open",
            CallContext::new(session.clone(), permissions.clone()),
            Value::Null,
        )
        .await
        .unwrap();
    assert_eq!(
        answer(open),
        json!("Allow"),
        "a command without a right is always given"
    );
}
