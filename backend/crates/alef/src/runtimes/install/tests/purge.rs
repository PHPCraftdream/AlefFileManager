// SPDX-License-Identifier: MIT OR Apache-2.0
use super::*;
use crate::{
    application::{support, trust},
    install::{
        integration::{Artifact, Integration},
        uninstall::uninstall_in,
    },
};
use alef_core::security::{
    authority::Authority,
    consent::{Consent, ConsentStore, FileConsentStore, Identity},
};
use sha2::{Digest, Sha256};
use std::{
    io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        loop {
            let path = std::env::temp_dir().join(format!(
                "alef-purge-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match std::fs::create_dir(&path) {
                Ok(()) => return Self(std::fs::canonicalize(&path).expect("canonical scratch")),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("{error}"),
            }
        }
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[derive(Default)]
struct Fake {
    installed: bool,
}

impl Integration for Fake {
    fn install(&mut self, _: &Artifact) -> io::Result<()> {
        self.installed = true;
        Ok(())
    }

    fn remove(&mut self, _: &str, _: Option<&str>) -> io::Result<bool> {
        let was_installed = self.installed;
        self.installed = false;
        Ok(was_installed)
    }
}

fn bases(scratch: &Scratch) -> (PathBuf, PathBuf, PathBuf) {
    (
        scratch.0.join("data"),
        scratch.0.join("config"),
        scratch.0.join("cache"),
    )
}

fn app_dir(home: &Path, id: &str) -> PathBuf {
    home.join("apps")
        .join(format!("{:x}", Sha256::digest(id.as_bytes())))
}

fn run(
    scratch: &Scratch,
    id: &str,
    purge: bool,
    fake: &mut Fake,
    announced: &mut Vec<PathBuf>,
) -> Result<(), InstallError> {
    let mut announce = |path: &Path| {
        assert!(
            path.exists(),
            "announced path must exist before deletion: {path:?}"
        );
        announced.push(path.to_owned());
        Ok(())
    };
    uninstall_in(
        &scratch.0.join("home"),
        id,
        purge,
        fake,
        &bases(scratch),
        &mut announce,
    )
}

fn identity(id: &str, fingerprint: &str) -> Identity {
    Identity {
        app_id: id.to_owned(),
        signer: Some(format!("pkg-signed:{fingerprint}")),
    }
}

fn real_pin(home: &Path, id: &str) -> trust::Pin {
    trust::pin(&home.join("trust"), id, &support::test_key_hex()).unwrap()
}

fn package_file(home: &Path, id: &str) -> (PathBuf, String) {
    let manifest = support::manifest_text("").replace("id: org.example.app", &format!("id: {id}"));
    let (bytes, fingerprint) = support::signed_package(&[
        ("alef.ktav", manifest.as_bytes()),
        ("dist/index.html", b"<html></html>"),
    ]);
    let dir = app_dir(home, id);
    std::fs::create_dir_all(&dir).unwrap();
    std::fs::write(dir.join("1.0.0.alef"), bytes).unwrap();
    (dir, fingerprint)
}

#[test]
fn ordinary_uninstall_removes_package_and_preserves_owned_state() {
    let scratch = Scratch::new();
    let id = "org.example.app";
    let home = scratch.0.join("home");
    let (dir, fingerprint) = package_file(&home, id);
    let pin = real_pin(&home, id);
    assert_eq!(pin.fingerprint, fingerprint);

    let app_identity = identity(id, &fingerprint);
    let store = FileConsentStore::new(home.join("consent"));
    store.save(&app_identity, &Consent::undecided()).unwrap();
    let shadow = home.join("shadow").join(app_identity.key());
    std::fs::create_dir_all(&shadow).unwrap();
    std::fs::write(shadow.join("keep"), b"state").unwrap();

    let mut fake = Fake { installed: true };
    let mut announced = Vec::new();
    run(&scratch, id, false, &mut fake, &mut announced).unwrap();

    assert!(!dir.exists());
    assert!(trust::load(&home.join("trust"), id).unwrap().is_some());
    assert!(store.load(&app_identity).unwrap().is_some());
    assert!(shadow.join("keep").exists());
    assert!(announced.is_empty());
}

#[test]
fn purge_removes_data_config_cache_identity_and_trust_but_keeps_legacy_and_other_identity() {
    let scratch = Scratch::new();
    let id = "org.example.app";
    let home = scratch.0.join("home");
    let (app, fingerprint) = package_file(&home, id);
    let pin = real_pin(&home, id);
    assert_eq!(pin.fingerprint, fingerprint);

    let app_identity = identity(id, &fingerprint);
    let authority = Authority::SignedPackage {
        fingerprint: fingerprint.clone(),
    };
    let token = authority.package_token(id).unwrap();
    let other_token = authority.package_token("org.other.app").unwrap();
    let (data, config, cache) = bases(&scratch);
    let store = FileConsentStore::new(home.join("consent"));
    store.save(&app_identity, &Consent::undecided()).unwrap();

    let paths = [
        data.join("alef-packages").join(&token),
        config.join("alef-packages").join(&token),
        cache.join("alef-packages").join(&token),
        home.join("shadow").join(app_identity.key()),
    ];
    for path in &paths {
        std::fs::create_dir_all(path).unwrap();
        std::fs::write(path.join("keep"), b"state").unwrap();
    }
    let other_shadow = home.join("shadow").join(other_token);
    std::fs::create_dir_all(&other_shadow).unwrap();
    std::fs::write(other_shadow.join("keep"), b"other").unwrap();
    let legacy = data.join(id);
    std::fs::create_dir_all(&legacy).unwrap();
    std::fs::write(legacy.join("keep"), b"legacy").unwrap();

    let mut fake = Fake { installed: true };
    let mut announced = Vec::new();
    run(&scratch, id, true, &mut fake, &mut announced).unwrap();

    assert!(!app.exists());
    assert!(trust::load(&home.join("trust"), id).unwrap().is_none());
    assert!(store.load(&app_identity).unwrap().is_none());
    let consent_path = home
        .join("consent")
        .join(format!("{}.json", app_identity.key()));
    let pin_path = trust::pin_path(&home.join("trust"), id);
    let mut expected = paths.to_vec();
    expected.push(consent_path);
    expected.push(pin_path);
    for path in &expected {
        assert!(!path.exists(), "purged path remains: {path:?}");
        assert!(announced.contains(path), "path was not announced: {path:?}");
    }
    assert!(announced.iter().all(|path| !path.exists()));
    assert!(other_shadow.join("keep").exists());
    assert!(legacy.join("keep").exists());
}

#[test]
fn half_written_application_directory_is_cleaned_successfully() {
    let scratch = Scratch::new();
    let id = "org.example.partial";
    let app = app_dir(&scratch.0.join("home"), id);
    std::fs::create_dir_all(&app).unwrap();
    std::fs::write(app.join("partial"), b"not a package artifact").unwrap();

    let mut fake = Fake::default();
    let mut announced = Vec::new();
    run(&scratch, id, false, &mut fake, &mut announced).unwrap();

    assert!(!app.exists());
    assert!(announced.is_empty());
}

#[test]
fn orphaned_integration_artifact_is_cleaned_successfully() {
    let scratch = Scratch::new();
    let id = "org.example.orphan";
    let mut fake = Fake { installed: true };
    let mut announced = Vec::new();

    run(&scratch, id, false, &mut fake, &mut announced).unwrap();

    assert!(!app_dir(&scratch.0.join("home"), id).exists());
    assert!(!fake.installed);
    assert!(announced.is_empty());
}

#[test]
fn unknown_application_without_directory_or_artifact_is_not_installed() {
    let scratch = Scratch::new();
    let id = "org.unknown.app";
    let mut fake = Fake::default();
    let mut announced = Vec::new();

    let error = run(&scratch, id, false, &mut fake, &mut announced).unwrap_err();

    assert_eq!(error.category, "NOT_INSTALLED");
    assert!(!app_dir(&scratch.0.join("home"), id).exists());
    assert!(announced.is_empty());
}
