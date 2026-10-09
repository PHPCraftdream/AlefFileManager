// SPDX-License-Identifier: MIT OR Apache-2.0
//! Permission gating: closed-by-default booleans, environment names, window creation and error hygiene.
mod commands;
mod consent;

use alef_core::{
    security::{
        grants::Grants,
        manifest::{Manifest, Permissions},
        permissions::{PathVars, Permission, PermissionSet},
    },
    ErrorCode,
};
use std::path::PathBuf;

const ALL: [Permission; 15] = [
    Permission::None,
    Permission::FsRead,
    Permission::FsWrite,
    Permission::CliExec,
    Permission::CliCommand,
    Permission::NetHttp,
    Permission::NetSocket,
    Permission::ShellOpenExternal,
    Permission::ClipboardRead,
    Permission::ShortcutGlobal,
    Permission::Secrets,
    Permission::AppEnv,
    Permission::AppDeepLinks,
    Permission::AppAutostart,
    Permission::WindowCreate,
];

fn closed() -> Permissions {
    Manifest::from_ktav_str(include_str!("../fixtures/minimal.ktav"))
        .expect("fixture")
        .permissions
}

fn vars() -> PathVars {
    let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR"));
    PathVars {
        app_data: root.clone(),
        app_config: root.clone(),
        app_cache: root.clone(),
        home: root.clone(),
        documents: root.clone(),
        downloads: root.clone(),
        desktop: root.clone(),
        temp: root.clone(),
        app: root,
    }
}

fn build(policy: &Permissions) -> PermissionSet {
    PermissionSet::from_manifest(policy, &vars()).expect("valid policy")
}

fn allowed(set: &PermissionSet, permission: Permission, target: Option<&str>) -> bool {
    set.check(permission, target, &Grants::new()).is_ok()
}

#[test]
fn closed_manifest_denies_everything_except_none() {
    let set = build(&closed());
    for permission in ALL {
        for target in [None, Some("anything"), Some("")] {
            assert_eq!(
                allowed(&set, permission, target),
                permission == Permission::None,
                "{permission:?} {target:?}"
            );
        }
    }
}

#[test]
fn each_boolean_permission_opens_only_itself() {
    type Open = fn(&mut Permissions);
    let cases: [(Permission, Open); 4] = [
        (Permission::ClipboardRead, |p| p.clipboard.read = true),
        (Permission::ShortcutGlobal, |p| p.shortcut.global = true),
        (Permission::Secrets, |p| p.secrets = true),
        (Permission::AppAutostart, |p| p.app.autostart = true),
    ];
    for (opened, open) in cases {
        let mut policy = closed();
        open(&mut policy);
        let set = build(&policy);
        for permission in ALL {
            let expected = permission == Permission::None || permission == opened;
            assert_eq!(
                allowed(&set, permission, None),
                expected,
                "{opened:?} opens {permission:?}"
            );
        }
    }
}

#[test]
fn app_env_matches_exact_names_only() {
    let mut policy = closed();
    policy.app.env = vec!["HOME".into(), "MY_APP_MODE".into()];
    let set = build(&policy);
    assert!(allowed(&set, Permission::AppEnv, Some("HOME")));
    assert!(allowed(&set, Permission::AppEnv, Some("MY_APP_MODE")));
    for no in ["home", "HOM", "HOME2", "MY_APP", "", "HOME=x", "HOME\0"] {
        assert!(!allowed(&set, Permission::AppEnv, Some(no)), "{no:?}");
    }
    assert!(!allowed(&set, Permission::AppEnv, None));
    for bad in ["", "A=B", "A\0B", "A\nB"] {
        policy.app.env = vec![bad.into()];
        assert_eq!(
            PermissionSet::from_manifest(&policy, &vars())
                .unwrap_err()
                .code,
            ErrorCode::ManifestInvalid,
            "{bad:?}"
        );
    }
}

#[test]
fn window_create_needs_the_manifest_section_or_the_embedder() {
    use alef_core::security::manifest::WindowPermissions;
    let mut policy = closed();
    assert!(policy.window.is_none(), "the fixture has no window section");
    for (window, expected) in [
        (None, false),
        (Some(WindowPermissions { create: false }), false),
        (Some(WindowPermissions { create: true }), true),
    ] {
        policy.window = window;
        let set = build(&policy);
        assert_eq!(allowed(&set, Permission::WindowCreate, None), expected);
        assert!(
            !allowed(&set, Permission::Secrets, None),
            "the window section opens nothing else"
        );
    }
    let set = build(&closed());
    assert!(!allowed(&set, Permission::WindowCreate, None));
    let opened = set.clone().with_window_create();
    assert!(allowed(&opened, Permission::WindowCreate, None));
    assert!(
        !allowed(&set, Permission::WindowCreate, None),
        "the original stays closed"
    );
    assert!(
        !allowed(&opened, Permission::Secrets, None),
        "opt-in opens nothing else"
    );
}

#[test]
fn denial_names_the_permission_but_never_the_target() {
    let mut policy = closed();
    policy.secrets = false;
    let set = build(&policy);
    for (permission, name) in [
        (Permission::Secrets, "secrets"),
        (Permission::FsRead, "fs.read"),
        (Permission::NetHttp, "net.http"),
        (Permission::ShellOpenExternal, "shell.openExternal"),
        (Permission::AppEnv, "app.env"),
        (Permission::AppDeepLinks, "app.deepLinks"),
        (Permission::AppAutostart, "app.autostart"),
    ] {
        let target = "https://super-secret-token-123.example.com/path";
        let error = set
            .check(permission, Some(target), &Grants::new())
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::PermissionDenied);
        assert_eq!(error.details.as_ref().expect("details")["permission"], name);
        let rendered = format!("{error:?} {}", serde_json::to_string(&error).expect("json"));
        assert!(!rendered.contains("super-secret-token-123"), "{rendered}");
    }
}

#[test]
fn deep_links_declarations_gate_consent_and_narrow_every_holder() {
    use alef_core::security::consent::{Consent, Decision, Right};
    let manifest = Manifest::from_ktav_str(&format!(
        "{}\ndeepLinks: [\n    :: alef\n    :: notes\n]\n",
        include_str!("../fixtures/minimal.ktav")
    ))
    .unwrap();
    let base = build(&manifest.permissions)
        .with_deep_links(&manifest.deep_links)
        .unwrap();
    assert_eq!(
        base.rights(),
        [
            Right::scoped("app.deepLinks", "alef"),
            Right::scoped("app.deepLinks", "notes")
        ]
    );
    let check =
        |set: &PermissionSet, target| set.check(Permission::AppDeepLinks, target, &Grants::new());
    for decision in [Decision::Allow, Decision::Substitute, Decision::Deny] {
        let mut consent = Consent::allow_all();
        consent.set(Right::scoped("app.deepLinks", "alef"), decision);
        let set = base.clone().with_consent(consent);
        match decision {
            Decision::Deny => assert_eq!(
                check(&set, Some("alef")).unwrap_err().code,
                ErrorCode::PermissionDenied
            ),
            _ => assert_eq!(check(&set, Some("alef")).unwrap(), decision),
        }
        assert_eq!(check(&set, Some("notes")).unwrap(), Decision::Allow);
        for target in [
            None,
            Some(""),
            Some("ALEF"),
            Some("alef:"),
            Some("alef://path"),
            Some("undeclared"),
        ] {
            let denied = check(&set, target).unwrap_err();
            assert_eq!(
                denied.details,
                Some(serde_json::json!({"permission": "app.deepLinks"}))
            );
        }
    }
    assert!(check(
        &base.clone().with_consent(Consent::undecided()),
        Some("alef")
    )
    .is_err());
    assert!(check(
        &build(&closed()).with_consent(Consent::allow_all()),
        Some("alef")
    )
    .is_err());
    let holder = base.clone();
    let mut consent = Consent::undecided();
    consent.set(Right::scoped("app.deepLinks", "alef"), Decision::Substitute);
    assert!(base.narrow(&consent));
    for set in [&base, &holder] {
        assert_eq!(check(set, Some("alef")).unwrap(), Decision::Substitute);
        assert_eq!(check(set, Some("notes")).unwrap(), Decision::Allow);
    }
    consent.set(Right::scoped("app.deepLinks", "alef"), Decision::Allow);
    assert!(!base.narrow(&consent));
    consent.set(Right::scoped("app.deepLinks", "alef"), Decision::Deny);
    assert!(base.narrow(&consent));
    for set in [&base, &holder] {
        assert!(check(set, Some("alef")).is_err());
    }
    consent.set(Right::scoped("app.deepLinks", "alef"), Decision::Allow);
    assert!(!base.narrow(&consent));
    for schemes in [
        vec!["HTTP".into()],
        vec!["http".into()],
        vec!["alef".into(), "alef".into()],
        vec!["a".into(); 9],
    ] {
        assert_eq!(
            build(&closed()).with_deep_links(&schemes).unwrap_err().code,
            ErrorCode::ManifestInvalid
        );
    }
}

#[test]
fn autostart_obeys_consent_and_live_narrowing_in_every_holder() {
    use alef_core::security::consent::{Consent, Decision, Right};
    let consent = |decision| {
        let mut consent = Consent::undecided();
        consent.set(Right::plain("app.autostart"), decision);
        consent
    };
    let check =
        |set: &PermissionSet, permission, target| set.check(permission, target, &Grants::new());
    let right = Right::plain("app.autostart");
    let permissions = {
        let mut policy = closed();
        policy.app.autostart = true;
        build(&policy)
    };
    assert_eq!(permissions.rights(), [right]);
    assert_eq!(
        check(&permissions, Permission::AppAutostart, None).unwrap(),
        Decision::Allow
    );
    let holder = permissions.clone();
    assert!(permissions.narrow(&consent(Decision::Substitute)));
    for set in [&permissions, &holder] {
        assert_eq!(
            check(set, Permission::AppAutostart, None).unwrap(),
            Decision::Substitute
        );
    }
    assert!(!permissions.narrow(&consent(Decision::Allow)));
    assert!(permissions.narrow(&consent(Decision::Deny)));
    let undeclared = build(&closed()).with_consent(Consent::allow_all());
    let denied = check(&undeclared, Permission::AppAutostart, None).unwrap_err();
    assert_eq!(
        denied.details,
        Some(serde_json::json!({"permission": "app.autostart"}))
    );
    for set in [&permissions, &holder] {
        assert_eq!(
            check(set, Permission::AppAutostart, None).unwrap_err(),
            denied
        );
    }
    assert!(!permissions.narrow(&consent(Decision::Allow)));
}
