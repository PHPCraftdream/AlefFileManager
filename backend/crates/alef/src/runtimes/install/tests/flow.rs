// SPDX-License-Identifier: MIT OR Apache-2.0
use super::*;
use crate::application::trust::{TrustAsker, TrustInputs};
use crate::{
    application::{source, support, trust},
    install::{
        flow::{install_in, InstallOutcome},
        integration::{Artifact, Integration},
        store::AppLock,
    },
};
use sha2::Digest;
use std::{
    collections::BTreeMap,
    io,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicU64, Ordering},
        mpsc,
    },
    time::Duration,
};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        loop {
            let path = std::env::temp_dir().join(format!(
                "alef-flow-{}-{}",
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

const ICON: &[u8] = include_bytes!("../../../../../../assets/logo-32x32.png");

fn fixture(version: &str, body: &[u8]) -> (Vec<u8>, String) {
    let manifest =
        support::manifest_text("").replace("version:: 1.0.0", &format!("version:: {version}"));
    support::signed_package(&[
        ("alef.ktav", manifest.as_bytes()),
        ("dist/index.html", body),
        ("icon.png", ICON),
    ])
}

fn fixture_without_icon(version: &str, body: &[u8]) -> (Vec<u8>, String) {
    let manifest =
        support::manifest_text("").replace("version:: 1.0.0", &format!("version:: {version}"));
    support::signed_package(&[
        ("alef.ktav", manifest.as_bytes()),
        ("dist/index.html", body),
    ])
}

struct Fake {
    artifacts: BTreeMap<String, Artifact>,
    fail_after_install: bool,
    fail_record_publish: bool,
    publication_path: Option<PathBuf>,
    observed: Option<(bool, bool)>,
}

impl Fake {
    fn new() -> Self {
        Self {
            artifacts: BTreeMap::new(),
            fail_after_install: false,
            fail_record_publish: false,
            publication_path: None,
            observed: None,
        }
    }
}

impl Integration for Fake {
    fn install(&mut self, artifact: &Artifact) -> io::Result<()> {
        if let Some(parent) = artifact.package.parent() {
            self.observed = Some((
                parent.join("1.0.0.alef").exists(),
                artifact.package.exists(),
            ));
        }
        if self.fail_after_install {
            self.fail_after_install = false;
            self.artifacts.insert(artifact.id.clone(), artifact.clone());
            return Err(io::Error::other("injected after artifact creation"));
        }
        self.artifacts.insert(artifact.id.clone(), artifact.clone());
        if self.fail_record_publish {
            self.fail_record_publish = false;
            let record = artifact.package.parent().unwrap().join("app.ktav");
            std::fs::create_dir(&record)?;
            self.publication_path = Some(record);
        }
        Ok(())
    }

    fn remove(&mut self, id: &str, _: Option<&str>) -> io::Result<bool> {
        if let Some(artifact) = self.artifacts.get(id) {
            let record = artifact.package.parent().unwrap().join("app.ktav");
            if std::fs::symlink_metadata(&record).is_ok_and(|meta| meta.is_dir()) {
                std::fs::remove_dir(&record)?;
            }
        }
        if let Some(record) = self.publication_path.take() {
            let _ = std::fs::remove_dir_all(record);
        }
        Ok(self.artifacts.remove(id).is_some())
    }
}

async fn perform(
    home: &Path,
    bytes: Vec<u8>,
    key: &str,
    fake: &mut Fake,
) -> Result<InstallOutcome, InstallError> {
    perform_with_inputs(
        home,
        bytes,
        TrustInputs {
            trust_key: Some(key.into()),
            allow_unsigned: false,
        },
        fake,
    )
    .await
}

async fn perform_with_inputs(
    home: &Path,
    bytes: Vec<u8>,
    inputs: TrustInputs,
    fake: &mut Fake,
) -> Result<InstallOutcome, InstallError> {
    let path = home.join(format!(
        "source-{}.alef",
        NEXT.fetch_add(1, Ordering::Relaxed)
    ));
    std::fs::write(&path, bytes).unwrap();
    let (package, snapshot) = source::load_install(&path).unwrap();
    let _ = std::fs::remove_file(path);
    install_in(
        &home.join("runtime"),
        &std::env::current_exe().unwrap(),
        package,
        snapshot,
        &inputs,
        &TrustAsker::Scripted(true),
        fake,
    )
    .await
}

fn appdir(home: &Path) -> PathBuf {
    home.join("runtime/apps")
        .join(format!("{:x}", sha2::Sha256::digest(b"org.example.app")))
}

fn record(home: &Path) -> AppRecord {
    read_record(
        &appdir(home),
        &format!("{:x}", sha2::Sha256::digest(b"org.example.app")),
    )
    .unwrap()
}

#[tokio::test]
async fn unsigned_install_and_empty_name_use_id_for_record_and_artifact() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let scratch = Scratch::new();
        let manifest = support::manifest_text("").replace("name: Example", "name::");
        let bytes = support::unsigned_package(&[
            ("alef.ktav", manifest.as_bytes()),
            ("dist/index.html", b"<html/>"),
        ]);
        let mut fake = Fake::new();
        let outcome = perform_with_inputs(
            &scratch.0,
            bytes,
            TrustInputs {
                trust_key: None,
                allow_unsigned: true,
            },
            &mut fake,
        )
        .await
        .unwrap();
        let installed = match outcome {
            InstallOutcome::Installed(record) => record,
            _ => panic!("new unsigned package must install"),
        };
        assert_eq!(installed.name, "org.example.app");
        assert!(installed.icon.is_none());
        let hash = format!("{:x}", sha2::Sha256::digest(installed.id.as_bytes()));
        assert_eq!(
            trust::load(&scratch.0.join("runtime/trust"), &installed.id).unwrap(),
            None
        );
        assert_eq!(read_record(&appdir(&scratch.0), &hash).unwrap(), installed);
        let artifact = &fake.artifacts["org.example.app"];
        assert_eq!(artifact.name, installed.id);
        assert!(artifact.package.exists());
        assert_eq!(
            artifact.package,
            appdir(&scratch.0).join(&installed.package)
        );
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn identical_signed_reinstall_is_idempotent() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let scratch = Scratch::new();
        let (bytes, key) = fixture("1.0.0", b"<html>one</html>");
        let mut fake = Fake::new();
        assert!(matches!(
            perform(&scratch.0, bytes.clone(), &key, &mut fake)
                .await
                .unwrap(),
            InstallOutcome::Installed(_)
        ));
        assert!(matches!(
            perform(&scratch.0, bytes, &key, &mut fake).await.unwrap(),
            InstallOutcome::AlreadyInstalled(_)
        ));
        assert_eq!(fake.artifacts.len(), 1);
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn invalid_entry_is_rejected_without_mutation() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let scratch = Scratch::new();
        let manifest = support::manifest_text("");
        let bad = support::signed_package(&[
            ("alef.ktav", manifest.as_bytes()),
            ("dist/other.html", b"<html/>"),
        ]);
        let mut fake = Fake::new();
        assert_eq!(
            perform(&scratch.0, bad.0, &bad.1, &mut fake)
                .await
                .unwrap_err()
                .category,
            "MANIFEST_INVALID"
        );
        assert!(!appdir(&scratch.0).exists());
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn changed_signer_is_refused_without_creating_application() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let scratch = Scratch::new();
        let app_id = "org.example.app";
        let alternate = ed25519_dalek::SigningKey::from_bytes(&[43; 32]);
        let alternate_key: String = alternate
            .verifying_key()
            .as_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        let pin = trust::pin(&scratch.0.join("runtime/trust"), app_id, &alternate_key).unwrap();
        let (candidate, candidate_key) = fixture("1.0.0", b"<html/> ");
        assert_ne!(candidate_key, alternate_key);
        let mut fake = Fake::new();
        assert_eq!(
            perform(&scratch.0, candidate, &candidate_key, &mut fake)
                .await
                .unwrap_err()
                .category,
            "SIGNER_CHANGED"
        );
        assert!(!appdir(&scratch.0).exists());
        assert_eq!(
            trust::load(&scratch.0.join("runtime/trust"), app_id)
                .unwrap()
                .unwrap(),
            pin
        );
        assert!(fake.artifacts.is_empty());
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn unsigned_update_requires_signature_even_with_allow_unsigned() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let scratch = Scratch::new();
        let (bytes, key) = fixture("1.0.0", b"<html/> ");
        let mut fake = Fake::new();
        perform(&scratch.0, bytes, &key, &mut fake).await.unwrap();
        let before = record(&scratch.0);
        let manifest = support::manifest_text("").replace("version:: 1.0.0", "version:: 2.0.0");
        let unsigned = support::unsigned_package(&[
            ("alef.ktav", manifest.as_bytes()),
            ("dist/index.html", b"<html/>"),
        ]);
        assert_eq!(
            perform_with_inputs(
                &scratch.0,
                unsigned,
                TrustInputs {
                    trust_key: None,
                    allow_unsigned: true,
                },
                &mut fake,
            )
            .await
            .unwrap_err()
            .category,
            "SIGNATURE_REQUIRED"
        );
        assert_eq!(record(&scratch.0), before);
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn equal_version_different_bytes_and_lower_version_are_refused_without_mutation() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let scratch = Scratch::new();
        let (initial, key) = fixture("2.0.0", b"<html>original</html>");
        let mut fake = Fake::new();
        perform(&scratch.0, initial, &key, &mut fake).await.unwrap();
        let before = record(&scratch.0);
        let package_path = appdir(&scratch.0).join(&before.package);
        let bytes = std::fs::read(&package_path).unwrap();
        let artifact = fake.artifacts["org.example.app"].clone();

        let (same_version, _) = fixture("2.0.0", b"<html>different</html>");
        assert_eq!(
            perform(&scratch.0, same_version, &key, &mut fake)
                .await
                .unwrap_err()
                .category,
            "ALREADY_INSTALLED"
        );
        let (lower_version, _) = fixture("1.0.0", b"<html>older</html>");
        assert_eq!(
            perform(&scratch.0, lower_version, &key, &mut fake)
                .await
                .unwrap_err()
                .category,
            "DOWNGRADE"
        );
        assert_eq!(record(&scratch.0), before);
        assert_eq!(std::fs::read(package_path).unwrap(), bytes);
        assert_eq!(fake.artifacts["org.example.app"].package, artifact.package);
        assert_eq!(std::fs::read(&artifact.package).unwrap(), bytes);
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn failed_initial_install_cleans_directory_and_upgrade_failure_restores_package_metadata_and_icon(
) {
    tokio::time::timeout(Duration::from_secs(10), async {
        let scratch = Scratch::new();
        let (initial, key) = fixture("1.0.0", b"<html>old</html>");
        let mut fake = Fake::new();
        fake.fail_after_install = true;
        assert_eq!(
            perform(&scratch.0, initial.clone(), &key, &mut fake)
                .await
                .unwrap_err()
                .category,
            "APP_STORE"
        );
        assert!(!appdir(&scratch.0).exists());
        assert!(fake.artifacts.is_empty());

        fake.fail_record_publish = true;
        let failure = perform(&scratch.0, initial, &key, &mut fake)
            .await
            .unwrap_err();
        assert_eq!(failure.category, "APP_STORE");
        assert!(fake.artifacts.is_empty());
        assert!(!appdir(&scratch.0).exists());
        assert!(scratch.0.join("runtime/apps").exists());

        let (initial, _) = fixture("1.0.0", b"<html>old</html>");
        perform(&scratch.0, initial, &key, &mut fake).await.unwrap();
        let before = record(&scratch.0);
        let package = appdir(&scratch.0).join(&before.package);
        let old_artifact = fake.artifacts["org.example.app"].clone();
        let bytes = std::fs::read(&package).unwrap();
        let icon = std::fs::read(appdir(&scratch.0).join("icon.png")).unwrap();
        let (upgrade, _) = fixture_without_icon("2.0.0", b"<html>new</html>");
        fake.fail_after_install = true;
        assert_eq!(
            perform(&scratch.0, upgrade, &key, &mut fake)
                .await
                .unwrap_err()
                .category,
            "APP_STORE"
        );
        assert_eq!(record(&scratch.0), before);
        assert_eq!(std::fs::read(&package).unwrap(), bytes);
        assert_eq!(
            std::fs::read(appdir(&scratch.0).join("icon.png")).unwrap(),
            icon
        );
        assert!(appdir(&scratch.0).join("1.0.0.alef").exists());
        assert!(!appdir(&scratch.0).join("2.0.0.alef").exists());
        assert_eq!(fake.artifacts.len(), 1);
        assert_eq!(fake.artifacts["org.example.app"].package, package);
        assert_eq!(fake.artifacts["org.example.app"], old_artifact);
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn upgrade_to_a_package_without_an_icon_succeeds_and_removes_the_old_icon() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let scratch = Scratch::new();
        let (initial, key) = fixture("1.0.0", b"<html>old</html>");
        let mut fake = Fake::new();
        perform(&scratch.0, initial, &key, &mut fake).await.unwrap();
        assert!(appdir(&scratch.0).join("icon.png").exists());
        let (upgrade, _) = fixture_without_icon("2.0.0", b"<html>new</html>");
        let outcome = perform(&scratch.0, upgrade, &key, &mut fake).await.unwrap();
        let record = match outcome {
            InstallOutcome::Installed(record) => record,
            _ => panic!(),
        };
        assert_eq!(record.version, "2.0.0");
        assert!(!appdir(&scratch.0).join("icon.png").exists());
        assert!(!appdir(&scratch.0).join("1.0.0.alef").exists());
        assert!(appdir(&scratch.0).join(&record.package).exists());
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn upgrade_observes_both_artifacts_and_publishes_new_record() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let scratch = Scratch::new();
        let (initial, key) = fixture("1.0.0", b"<html>old</html>");
        let mut fake = Fake::new();
        perform(&scratch.0, initial, &key, &mut fake).await.unwrap();
        let (upgrade, _) = fixture("2.0.0", b"<html>new</html>");
        let upgrade_bytes = upgrade.clone();
        let outcome = perform(&scratch.0, upgrade, &key, &mut fake).await.unwrap();
        assert_eq!(fake.observed, Some((true, true)));
        let record = match outcome {
            InstallOutcome::Installed(record) => record,
            _ => panic!(),
        };
        assert_eq!(record.version, "2.0.0");
        assert!(!appdir(&scratch.0).join("1.0.0.alef").exists());
        assert_eq!(
            std::fs::read(appdir(&scratch.0).join(&record.package)).unwrap(),
            upgrade_bytes
        );
        assert_eq!(
            fake.artifacts["org.example.app"].package,
            appdir(&scratch.0).join(&record.package)
        );
    })
    .await
    .unwrap();
}

#[test]
fn concurrent_identical_installs_publish_exactly_one_record_and_package() {
    let scratch = Scratch::new();
    let (bytes, key) = fixture("1.0.0", b"<html>race</html>");
    let home = scratch.0.clone();
    let mut start_senders = Vec::new();
    let (ready_send, ready_receive) = mpsc::sync_channel(2);
    let (send, receive) = mpsc::sync_channel(2);
    let mut workers = Vec::new();
    for _ in 0..2 {
        let bytes = bytes.clone();
        let key = key.clone();
        let home = home.clone();
        let (start_send, start_receive) = mpsc::sync_channel(1);
        start_senders.push(start_send);
        let ready_send = ready_send.clone();
        let send = send.clone();
        workers.push(std::thread::spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .unwrap();
            ready_send.send(()).unwrap();
            if start_receive.recv_timeout(Duration::from_secs(5)).is_err() {
                let _ = send.send(Err(InstallError::new(
                    "APP_STORE",
                    "start signal timed out",
                    1,
                )));
                return;
            }
            let mut fake = Fake::new();
            let result = runtime.block_on(perform(&home, bytes, &key, &mut fake));
            let _ = send.send(result.map(|outcome| match outcome {
                InstallOutcome::Installed(record) => (true, record),
                InstallOutcome::AlreadyInstalled(record) => (false, record),
            }));
        }));
    }
    ready_receive.recv_timeout(Duration::from_secs(5)).unwrap();
    ready_receive.recv_timeout(Duration::from_secs(5)).unwrap();
    for start in start_senders {
        start.send(()).unwrap();
    }
    drop(ready_send);
    drop(send);
    let first = receive
        .recv_timeout(Duration::from_secs(10))
        .unwrap()
        .unwrap();
    let second = receive
        .recv_timeout(Duration::from_secs(10))
        .unwrap()
        .unwrap();
    for worker in workers {
        worker.join().unwrap();
    }
    let worker_outcomes = [first, second];
    assert_ne!(worker_outcomes[0].0, worker_outcomes[1].0);
    assert_eq!(worker_outcomes[0].1, worker_outcomes[1].1);
    let hash = format!(
        "{:x}",
        sha2::Sha256::digest(worker_outcomes[0].1.id.as_bytes())
    );
    let mut app_records = Vec::new();
    for entry in std::fs::read_dir(appdir(&scratch.0)).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_some_and(|ext| ext == "alef") {
            app_records.push(path);
        }
    }
    assert_eq!(app_records.len(), 1);
    assert_eq!(
        read_record(&appdir(&scratch.0), &hash).unwrap(),
        worker_outcomes[0].1
    );
    assert_eq!(
        app_records[0],
        appdir(&scratch.0).join(&worker_outcomes[0].1.package)
    );
}

#[tokio::test]
async fn held_stable_gate_returns_busy_without_touching_existing_record() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let scratch = Scratch::new();
        let (bytes, key) = fixture("1.0.0", b"<html/>");
        let mut fake = Fake::new();
        perform(&scratch.0, bytes, &key, &mut fake).await.unwrap();
        let before = record(&scratch.0);
        let root = scratch.0.join("runtime/apps");
        let gate = AppLock::acquire_named(
            &root,
            &format!(".{:x}.lock", sha2::Sha256::digest(b"org.example.app")),
            Duration::from_millis(10),
        )
        .unwrap();
        let (candidate, _) = fixture("2.0.0", b"<html/>");
        assert_eq!(
            perform(&scratch.0, candidate, &key, &mut fake)
                .await
                .unwrap_err()
                .category,
            "BUSY"
        );
        drop(gate);
        assert_eq!(record(&scratch.0), before);
    })
    .await
    .unwrap();
}
