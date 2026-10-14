// SPDX-License-Identifier: MIT OR Apache-2.0
//! The `updater.install` applier: the store install pinned to the running signer.
use super::*;
use crate::{
    application::support,
    install::{
        integration::{Artifact, Integration},
        StoreApplier,
    },
};
use alef_core::{AlefError, ErrorCode};
use alef_modules::net::updater::UpdateApplier;
use sha2::Digest as _;
use std::{io, path::Path, sync::Arc, time::Duration};

const WAIT: Duration = Duration::from_secs(10);

fn fixture(version: &str) -> (Vec<u8>, String) {
    let manifest =
        support::manifest_text("").replace("version:: 1.0.0", &format!("version:: {version}"));
    support::signed_package(&[
        ("alef.ktav", manifest.as_bytes()),
        ("dist/index.html", b"<html/>".as_slice()),
    ])
}

/// A package signed by a key other than the fixed support key: the record format is identical.
fn alt_signed_package(files: &[(&str, &[u8])]) -> (Vec<u8>, String) {
    use base64::{engine::general_purpose::STANDARD, Engine};
    use ed25519_dalek::{Signer as _, SigningKey};
    use sha2::Digest as _;
    let list = support::hashes(files);
    let key = SigningKey::from_bytes(&[7; 32]);
    let public = *key.verifying_key().as_bytes();
    let message = [b"alef-package-v1\n".as_slice(), &list].concat();
    let record = format!(
        "alef-package 1\nkey {}\nsig {}\n",
        STANDARD.encode(public),
        STANDARD.encode(key.sign(&message).to_bytes())
    )
    .into_bytes();
    let mut records = files.to_vec();
    records.push(("META-INF/hashes", &list));
    records.push(("META-INF/signature", &record));
    (
        support::zip(&records),
        format!("{:x}", sha2::Sha256::digest(&public)),
    )
}

/// A well-formed package whose signature record does not verify: the CRCs are valid, so the
/// failure is the signature check itself, not the archive integrity.
fn badly_signed_package(files: &[(&str, &[u8])]) -> Vec<u8> {
    use base64::{engine::general_purpose::STANDARD, Engine};
    use ed25519_dalek::{Signer as _, SigningKey};
    let list = support::hashes(files);
    let victim = SigningKey::from_bytes(&[7; 32]); // the public key named in the record
    let other = SigningKey::from_bytes(&[8; 32]); // the key that actually signed
    let message = [b"alef-package-v1\n".as_slice(), &list].concat();
    let record = format!(
        "alef-package 1\nkey {}\nsig {}\n",
        STANDARD.encode(victim.verifying_key().as_bytes()),
        STANDARD.encode(other.sign(&message).to_bytes())
    )
    .into_bytes();
    let mut records = files.to_vec();
    records.push(("META-INF/hashes", &list));
    records.push(("META-INF/signature", &record));
    support::zip(&records)
}

/// A fake integration whose artifact map is the oracle, shared behind an `Arc` so the test can
/// read it back after the applier (which owns the `Box<dyn Integration>`) ran.
#[derive(Clone, Default)]
struct Shared(Arc<std::sync::Mutex<FakeInner>>);

#[derive(Default)]
struct FakeInner {
    artifacts: std::collections::BTreeMap<String, Artifact>,
    fail_once: bool,
}

impl Shared {
    fn new(fail_once: bool) -> Self {
        Self(Arc::new(std::sync::Mutex::new(FakeInner {
            fail_once,
            ..Default::default()
        })))
    }
    /// The artifacts the integration currently points at, cloned in full for assertions.
    fn artifacts(&self) -> Vec<Artifact> {
        self.0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .artifacts
            .values()
            .cloned()
            .collect()
    }
}

impl Integration for Shared {
    fn install(&mut self, artifact: &Artifact) -> io::Result<()> {
        let mut inner = self.0.lock().unwrap_or_else(|e| e.into_inner());
        if inner.fail_once {
            inner.fail_once = false;
            return Err(io::Error::other("injected integration failure"));
        }
        inner
            .artifacts
            .insert(artifact.id.clone(), artifact.clone());
        Ok(())
    }

    fn remove(&mut self, id: &str, _: Option<&str>) -> io::Result<bool> {
        Ok(self
            .0
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .artifacts
            .remove(id)
            .is_some())
    }
}

fn appdir(home: &Path) -> std::path::PathBuf {
    let hash = format!("{:x}", sha2::Sha256::digest(b"org.example.app"));
    home.join("apps").join(&hash)
}

fn record(home: &Path) -> AppRecord {
    read_record(
        &appdir(home),
        &format!("{:x}", sha2::Sha256::digest(b"org.example.app")),
    )
    .unwrap()
}

fn applier(home: &Path, fingerprint: &str, fake: Shared) -> StoreApplier {
    StoreApplier::new(
        home.to_owned(),
        home.join("current alef"),
        fingerprint.to_owned(),
        Box::new(fake),
        alef_modules::Termination::default(),
    )
}

async fn apply(applier: &StoreApplier, bytes: &[u8]) -> Result<std::path::PathBuf, AlefError> {
    let scratch = tempfile::tempdir().unwrap();
    let package = scratch.path().join("offered.alef");
    std::fs::write(&package, bytes).unwrap();
    let result = applier.apply(&package).await;
    let _ = std::fs::remove_file(&package);
    result
}

#[tokio::test]
async fn applying_a_higher_signed_version_installs_and_removes_the_old_package() {
    tokio::time::timeout(WAIT, async {
        let home = tempfile::tempdir().unwrap();
        let (old, fingerprint) = fixture("1.0.0");
        let fake = Shared::default();
        let applier = Arc::new(applier(home.path(), &fingerprint, fake.clone()));
        let installed = apply(&applier, &old).await.unwrap();
        assert_eq!(record(home.path()).version, "1.0.0");
        assert!(installed.exists());
        let (new, _) = fixture("2.0.0");
        let installed = apply(&applier, &new).await.unwrap();
        assert_eq!(record(home.path()).version, "2.0.0");
        assert!(installed.exists());
        // The integration was re-pointed: only the new artifact remains.
        let installed_artifacts = fake.artifacts();
        assert_eq!(installed_artifacts.len(), 1);
        assert_eq!(
            installed_artifacts[0].package,
            appdir(home.path()).join("2.0.0.alef")
        );
        assert_eq!(
            installed_artifacts[0].executable,
            home.path().join("current alef")
        );
        assert_eq!(installed_artifacts[0].id, "org.example.app");
        assert_eq!(
            installed,
            home.path()
                .join("apps")
                .join(format!("{:x}", sha2::Sha256::digest(b"org.example.app")))
                .join("2.0.0.alef")
        );
        assert!(!home
            .path()
            .join("apps")
            .join(format!("{:x}", sha2::Sha256::digest(b"org.example.app")))
            .join("1.0.0.alef")
            .exists());
    })
    .await
    .expect("bounded");
}

#[tokio::test]
async fn a_package_signed_by_another_key_is_refused_without_changing_the_store() {
    tokio::time::timeout(WAIT, async {
        let home = tempfile::tempdir().unwrap();
        let (old, fingerprint) = fixture("1.0.0");
        let fake = Shared::default();
        let applier = Arc::new(applier(home.path(), &fingerprint, fake.clone()));
        apply(&applier, &old).await.unwrap();
        let manifest = support::manifest_text("").replace("version:: 1.0.0", "version:: 2.0.0");
        let (other, other_fingerprint) = alt_signed_package(&[
            ("alef.ktav", manifest.as_bytes()),
            ("dist/index.html", b"<html/>".as_slice()),
        ]);
        assert_ne!(fingerprint, other_fingerprint);
        let error = apply(&applier, &other).await.unwrap_err();
        assert_eq!(error.code, ErrorCode::UpdateKeyMismatch);
        // The store is exactly as before the refused update.
        assert_eq!(record(home.path()).version, "1.0.0");
        assert!(!appdir(home.path()).join("2.0.0.alef").exists());
    })
    .await
    .expect("bounded");
}

#[tokio::test]
async fn a_failure_after_publishing_rolls_back_to_the_previous_version() {
    tokio::time::timeout(WAIT, async {
        let home = tempfile::tempdir().unwrap();
        let (old, fingerprint) = fixture("1.0.0");
        let plain = Shared::default();
        let first = Arc::new(applier(home.path(), &fingerprint, plain.clone()));
        apply(&first, &old).await.unwrap();
        let fake = Shared::new(true);
        let failing = applier(home.path(), &fingerprint, fake.clone());
        let (new, _) = fixture("2.0.0");
        let error = apply(&failing, &new).await.unwrap_err();
        assert_eq!(error.code, ErrorCode::UpdateIo);
        // The store looks exactly as before the failed update.
        assert_eq!(record(home.path()).version, "1.0.0");
        assert!(appdir(home.path()).join("1.0.0.alef").exists());
        assert!(!appdir(home.path()).join("2.0.0.alef").exists());
        // The integration was restored to the previous artifact.
        let artifacts = fake.artifacts();
        assert_eq!(artifacts.len(), 1);
        assert_eq!(artifacts[0].package, appdir(home.path()).join("1.0.0.alef"));
    })
    .await
    .expect("bounded");
}

// The post-commit cleanup is best effort: a stale entry that cannot be removed never fails the
// committed update.
#[tokio::test]
async fn a_stale_entry_that_cannot_be_removed_does_not_fail_the_update() {
    tokio::time::timeout(WAIT, async {
        let home = tempfile::tempdir().unwrap();
        let (old, fingerprint) = fixture("1.0.0");
        let applier = Arc::new(applier(home.path(), &fingerprint, Shared::default()));
        apply(&applier, &old).await.unwrap();
        // A directory with a stale package name cannot be removed by the prune loop; the
        // update must still succeed and leave the stubborn entry alone.
        let stubborn = appdir(home.path()).join("0.9.0.alef");
        std::fs::create_dir(&stubborn).unwrap();
        let (new, _) = fixture("2.0.0");
        let installed = apply(&applier, &new).await.unwrap();
        assert!(installed.exists());
        assert!(stubborn.is_dir());
    })
    .await
    .expect("bounded");
}

// The load path is exercised once: a well-formed package whose signature record does not verify
// (the CRCs stay valid) maps to UpdateSignatureInvalid before anything touches the store.
#[tokio::test]
async fn a_package_with_a_broken_signature_maps_to_update_signature_invalid() {
    tokio::time::timeout(WAIT, async {
        let home = tempfile::tempdir().unwrap();
        let (_, fingerprint) = fixture("1.0.0");
        let applier = applier(home.path(), &fingerprint, Shared::default());
        let manifest = support::manifest_text("").replace("version:: 1.0.0", "version:: 2.0.0");
        let bad = badly_signed_package(&[
            ("alef.ktav", manifest.as_bytes()),
            ("dist/index.html", b"<html/>".as_slice()),
        ]);
        let error = apply(&applier, &bad).await.unwrap_err();
        assert_eq!(error.code, ErrorCode::UpdateSignatureInvalid);
    })
    .await
    .expect("bounded");
}

// An update must carry the pinned signature; unsigned is never a silent downgrade.
#[tokio::test]
async fn an_unsigned_package_is_refused_as_a_key_mismatch() {
    tokio::time::timeout(WAIT, async {
        let home = tempfile::tempdir().unwrap();
        let (_, fingerprint) = fixture("1.0.0");
        let applier = applier(home.path(), &fingerprint, Shared::default());
        let manifest = support::manifest_text("").replace("version:: 1.0.0", "version:: 2.0.0");
        let unsigned = support::unsigned_package(&[
            ("alef.ktav", manifest.as_bytes()),
            ("dist/index.html", b"<html/>".as_slice()),
        ]);
        let error = apply(&applier, &unsigned).await.unwrap_err();
        assert_eq!(error.code, ErrorCode::UpdateKeyMismatch);
    })
    .await
    .expect("bounded");
}
