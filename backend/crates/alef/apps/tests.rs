// SPDX-License-Identifier: MIT OR Apache-2.0
use super::*;
use crate::{
    application::support,
    consent::fixtures::{app, app_with_command},
};
use alef_core::security::consent::FileConsentStore;
use sha2::{Digest, Sha256};
use std::path::PathBuf;

#[test]
fn lists_development_metadata_and_serializes_summary() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = app(tmp.path(), "org.example.dev");
    let store = FileConsentStore::new(tmp.path().join("consent"));
    let index = AppsIndex::new(tmp.path(), &store);
    let identity = crate::consent::identity_of(&dir, "org.example.dev").unwrap();
    let clipboard: Right = "clipboard.read".parse().unwrap();
    let mut consent = Consent::undecided();
    consent.set(clipboard, Decision::Allow);
    store.save(&identity, &consent).unwrap();

    let entry = index
        .list()
        .unwrap()
        .into_iter()
        .find(|e| e.identity.app_id == "org.example.dev")
        .unwrap();
    assert_eq!(entry.source.as_deref(), Some("dev"));
    assert_eq!(entry.name.as_deref(), Some("Example"));
    assert_eq!(entry.version.as_deref(), Some("1.0.0"));
    assert_eq!(entry.summary.allow, 1);
    assert_eq!(entry.summary.undecided, 1);
    assert!(entry
        .rights
        .iter()
        .any(|r| r.right.to_string() == "app.env:HOME"));
    let encoded = serde_json::to_value(&entry).unwrap();
    assert_eq!(encoded["source"], "dev");
    assert_eq!(encoded["name"], "Example");
    assert_eq!(encoded["version"], "1.0.0");
    assert_eq!(encoded["summary"]["allow"], 1);
    assert_eq!(encoded["summary"]["undecided"], 1);
    assert!(encoded["rights"]
        .as_array()
        .unwrap()
        .iter()
        .any(|r| { r["right"]["permission"] == "app.env" && r["right"]["scope"] == "HOME" }));
}

#[test]
fn directory_identity_separates_same_id_and_rejects_unknown_or_unconfirmed_red_rights() {
    let tmp = tempfile::tempdir().unwrap();
    let a = app(&tmp.path().join("a"), "org.example.same");
    let b = app(&tmp.path().join("b"), "org.example.same");
    let store = FileConsentStore::new(tmp.path().join("consent"));
    let index = AppsIndex::new(tmp.path(), &store);
    let da = index.describe(&a).unwrap();
    let db = index.describe(&b).unwrap();
    assert_ne!(da.identity, db.identity);
    let clipboard: Right = "clipboard.read".parse().unwrap();
    let absent: Right = "tray".parse().unwrap();
    assert!(index.set(&a, &absent, Decision::Allow, false).is_err());
    assert!(store.load(&da.identity).unwrap().is_none());
    let red_dir = app_with_command(tmp.path(), "org.example.red");
    let red_id = index.describe(&red_dir).unwrap().identity;
    let red = index
        .describe(&red_dir)
        .unwrap()
        .rights
        .into_iter()
        .find(|r| r.red)
        .unwrap()
        .right;
    let error = index
        .set(&red_dir, &red, Decision::Allow, false)
        .unwrap_err();
    assert!(error.contains("--confirm-red"));
    assert!(store.load(&red_id).unwrap().is_none());
    index.set(&a, &clipboard, Decision::Allow, false).unwrap();
    index.set(&b, &clipboard, Decision::Deny, false).unwrap();
    assert_eq!(
        store
            .load(&da.identity)
            .unwrap()
            .unwrap()
            .decision(&clipboard),
        Decision::Allow
    );
    assert_eq!(
        store
            .load(&db.identity)
            .unwrap()
            .unwrap()
            .decision(&clipboard),
        Decision::Deny
    );
    assert_eq!(
        index
            .list()
            .unwrap()
            .iter()
            .filter(|entry| entry.identity.app_id == da.identity.app_id)
            .count(),
        2
    );
}

#[test]
fn updates_enforce_decision_expansion_and_reset_to_undecided() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = app(tmp.path(), "org.example.transitions");
    let store = FileConsentStore::new(tmp.path().join("consent"));
    let index = AppsIndex::new(tmp.path(), &store);
    let right: Right = "clipboard.read".parse().unwrap();
    index
        .set(&dir, &right, Decision::Substitute, false)
        .unwrap();
    index.set(&dir, &right, Decision::Allow, false).unwrap();
    index
        .set(&dir, &right, Decision::Substitute, false)
        .unwrap();
    index.set(&dir, &right, Decision::Deny, false).unwrap();
    index.reset(&dir).unwrap();
    let identity = index.describe(&dir).unwrap().identity;
    let consent = store.load(&identity).unwrap().unwrap();
    assert_eq!(consent.decisions().count(), 0);
    assert!(index
        .list()
        .unwrap()
        .iter()
        .all(|entry| entry.identity != identity));
    let listed = crate::consent::permissions::run_in(
        &crate::args::PermissionsCommand::List { app: None },
        &store,
        tmp.path(),
    )
    .unwrap();
    assert!(!listed.contains("org.example.transitions"));

    let red_dir = app_with_command(tmp.path(), "org.example.redtransition");
    let red = index
        .describe(&red_dir)
        .unwrap()
        .rights
        .into_iter()
        .find(|r| r.red)
        .unwrap()
        .right;
    index.set(&red_dir, &red, Decision::Allow, true).unwrap();
    index
        .set(&red_dir, &red, Decision::Substitute, false)
        .unwrap();
    assert!(index.set(&red_dir, &red, Decision::Allow, false).is_err());
    index.set(&red_dir, &red, Decision::Deny, false).unwrap();
    index.reset(&red_dir).unwrap();
    let rid = index.describe(&red_dir).unwrap().identity;
    assert!(!store.load(&rid).unwrap().unwrap().decided(&red));
}

#[test]
fn installed_signed_package_id_uses_recorded_path_and_identity() {
    let tmp = tempfile::tempdir().unwrap();
    let home = tmp.path().join("home");
    let id = "org.example.app";
    let manifest = support::manifest_text("runtime: ^9.0.0\n")
        .replace("read: false", "read: true")
        .replace("env: []", "env: [ HOME ]");
    let (bytes, fingerprint) = support::signed_package(&[
        ("alef.ktav", manifest.as_bytes()),
        ("dist/index.html", b"ok"),
    ]);
    let hash = format!("{:x}", Sha256::digest(id.as_bytes()));
    let dir = home.join("apps").join(&hash);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("1.0.0.alef"), bytes).unwrap();
    let record = crate::install::AppRecord {
        id: id.into(),
        name: "Example".into(),
        version: "1.0.0".into(),
        icon: None,
        package: "1.0.0.alef".into(),
        executable: std::env::current_exe().unwrap(),
        installed_time: 1,
    };
    std::fs::write(dir.join("app.json"), serde_json::to_vec(&record).unwrap()).unwrap();
    let pkg = dir.join("1.0.0.alef");
    assert!(matches!(
        crate::application::source::load(&pkg),
        Err(error) if error.category == Some("RUNTIME_INCOMPATIBLE")
    ));
    assert!(crate::application::source::load_metadata(&pkg).is_ok());
    let store = FileConsentStore::new(tmp.path().join("consent"));
    let index = AppsIndex::new(&home, &store);
    let entries = index.list().unwrap();
    let entry = entries.iter().find(|e| e.identity.app_id == id).unwrap();
    let list = crate::consent::permissions::run_in(
        &crate::args::PermissionsCommand::List {
            app: Some(PathBuf::from(id)),
        },
        &store,
        &home,
    )
    .unwrap();
    assert!(list.contains("clipboard.read"));

    assert_eq!(entry.source.as_deref(), Some("installed"));
    assert_eq!(entry.availability, Availability::Available);
    assert_eq!(entry.summary.undecided, 2);
    assert_eq!(entry.summary.allow, 0);
    assert_eq!(
        entry.signer_fingerprint.as_deref(),
        Some(fingerprint.as_str())
    );
    let right: Right = "clipboard.read".parse().unwrap();
    let described = index.describe(Path::new(id)).unwrap();
    assert_eq!(
        described.identity.signer.as_deref(),
        Some(format!("pkg-signed:{fingerprint}").as_str())
    );
    index
        .set(Path::new(id), &right, Decision::Substitute, false)
        .unwrap();
    assert_eq!(
        store
            .load(&described.identity)
            .unwrap()
            .unwrap()
            .decision(&right),
        Decision::Substitute
    );
    index.reset(Path::new(id)).unwrap();
    let list = crate::consent::permissions::run_in(
        &crate::args::PermissionsCommand::List {
            app: Some(PathBuf::from(id)),
        },
        &store,
        &home,
    )
    .unwrap();
    assert!(list.starts_with("org.example.app\n"));
    assert!(list.contains("undecided\tclipboard.read"));
    let set = crate::consent::permissions::run_in(
        &crate::args::PermissionsCommand::Set {
            app: PathBuf::from(id),
            right: right.clone(),
            decision: Decision::Allow,
            confirm_red: false,
        },
        &store,
        &home,
    )
    .unwrap();
    assert_eq!(set, "clipboard.read: allow\n");
    assert_eq!(
        store
            .load(&described.identity)
            .unwrap()
            .unwrap()
            .decision(&right),
        Decision::Allow
    );
    let reset = crate::consent::permissions::run_in(
        &crate::args::PermissionsCommand::Reset {
            app: PathBuf::from(id),
        },
        &store,
        &home,
    )
    .unwrap();
    assert!(reset.contains("org.example.app"));
    assert!(!store
        .load(&described.identity)
        .unwrap()
        .unwrap()
        .decided(&right));
    assert_eq!(
        store
            .load(&described.identity)
            .unwrap()
            .unwrap()
            .decisions()
            .count(),
        0
    );
    let no_decisions = crate::consent::permissions::run_in(
        &crate::args::PermissionsCommand::List { app: None },
        &store,
        &home,
    )
    .unwrap();
    assert_eq!(no_decisions, "no decisions yet\n");
    let remaining = index.list().unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].availability, Availability::Available);
    assert_eq!(remaining[0].summary.undecided, 2);
    assert_eq!(remaining[0].summary.allow, 0);
}

#[test]
fn unavailable_development_source_is_listed_but_cannot_set_identity() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = app(tmp.path(), "org.example.missing");
    let store = FileConsentStore::new(tmp.path().join("consent"));
    let index = AppsIndex::new(tmp.path(), &store);
    let identity = crate::consent::identity_of(&dir, "org.example.missing").unwrap();
    let clipboard: Right = "clipboard.read".parse().unwrap();
    let mut consent = Consent::undecided();
    consent.set(clipboard.clone(), Decision::Allow);
    store.save(&identity, &consent).unwrap();
    std::fs::remove_dir_all(&dir).unwrap();

    let entry = index
        .list()
        .unwrap()
        .into_iter()
        .find(|e| e.identity == identity)
        .unwrap();
    assert_eq!(entry.source.as_deref(), Some("decisions-only"));
    assert_eq!(entry.name, None);
    assert_eq!(entry.version, None);
    assert_eq!(entry.summary.allow, 1);
    assert_eq!(entry.availability, Availability::DecisionsOnly);
    assert!(index
        .set_identity(&identity, &clipboard, Decision::Deny, false)
        .is_err());
    index.reset_identity(&identity).unwrap();
    let reset = store.load(&identity).unwrap().unwrap();
    assert!(!reset.decided(&clipboard));
    assert_eq!(reset.decision(&clipboard), Decision::Deny);
}

#[test]
fn narrowing_permissions_resets_the_decision_to_deny() {
    let tmp = tempfile::tempdir().unwrap();
    let dir = app(tmp.path(), "org.example.narrow");
    let manifest = crate::plan::load_manifest(&dir).unwrap();
    let vars = crate::plan::path_vars(&dir, &manifest.id).unwrap();
    let plan = crate::plan::make_plan(&dir, manifest, &vars).unwrap();
    let right: Right = "clipboard.read".parse().unwrap();
    let mut consent = Consent::undecided();
    consent.set(right.clone(), Decision::Allow);
    let permissions = (*plan.permissions).clone().with_consent(consent);
    assert_eq!(permissions.consent().decision(&right), Decision::Allow);

    let store = FileConsentStore::new(tmp.path().join("consent"));
    let index = AppsIndex::new(tmp.path(), &store);
    index.set(&dir, &right, Decision::Allow, false).unwrap();
    index.reset(&dir).unwrap();
    let identity = index.describe(&dir).unwrap().identity;
    let stored = store.load(&identity).unwrap().unwrap();
    assert!(crate::consent::narrow_stored(&permissions, &stored));
    assert_eq!(permissions.consent().decision(&right), Decision::Deny);

    index.set(&dir, &right, Decision::Allow, false).unwrap();
    let expanded = store.load(&identity).unwrap().unwrap();
    assert!(!crate::consent::narrow_stored(&permissions, &expanded));
    assert_eq!(permissions.consent().decision(&right), Decision::Deny);
}
