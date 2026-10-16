// SPDX-License-Identifier: MIT OR Apache-2.0
//! The shared tail of the launch pipeline: the entry and window documents are checked through
//! the same lookup that serves requests, then trust and consent are settled. The check runs
//! before any question is asked or answer written.
use std::{num::NonZeroUsize, sync::Arc};

use alef_core::security::{
    consent::{Consent, FileConsentStore, Identity},
    permissions::PermissionSet,
};
use alef_core::session::BlockingExecutor;
use alef_runtime::{prepare_assets, PreparedAssets};
use url::Url;

use super::{
    plan::Plan,
    source::LoadFailure,
    trust::{decide_with_asker, trust_folder, TrustAsker, TrustFailure, TrustInputs},
};
use crate::consent::{
    ask::AppSummary, enforce, runtime_home, settle_with_async, store_decisions, stored_decisions,
    Unanswered,
};

/// Why the launch did not reach the runtime, before any window exists.
#[derive(Debug)]
pub enum SettleFailure {
    /// An entry or window document is missing or foreign; exit 2, before any question.
    Documents(LoadFailure),
    Trust(TrustFailure),
    /// Rights nobody decided on.
    Unanswered(Unanswered),
    Store(std::io::Error),
    /// An internal inconsistency of the pipeline itself.
    Enforcement(String),
}

/// What the pipeline hands to the runtime: the plan, the built asset source and the decisions.
pub struct SettledLaunch {
    pub plan: Plan,
    /// `None` only in development mode, which serves its own documents.
    pub assets: Option<PreparedAssets>,
    pub consent: Consent,
    pub granted: Arc<PermissionSet>,
}

/// Validates and prepares package documents before any trust question or trust-store write.
pub async fn prepare_documents(
    plan: &mut Plan,
    dev_url: Option<&Url>,
) -> Result<Option<PreparedAssets>, LoadFailure> {
    if dev_url.is_some() {
        return Ok(None);
    }
    let entry = plan.entry();
    let documents: Vec<String> = plan
        .manifest
        .windows
        .iter()
        .map(|window| window.url.clone())
        .collect();
    prepare_assets(plan.take_assets(), Some(&entry), &documents)
        .await
        .map(Some)
        .map_err(|error| LoadFailure {
            category: Some("MANIFEST_INVALID"),
            detail: format!("assets: {error}"),
            exit: 2,
            runtime_requirement: None,
        })
}

pub async fn settle_launch(
    mut plan: Plan,
    identity: &Identity,
    trust: Option<&(Option<(String, String)>, String, TrustInputs)>,
    trust_asker: Option<&TrustAsker>,
    consent_asker: &crate::consent::Asker,
    decisions: &FileConsentStore,
    dev_url: Option<&Url>,
) -> Result<SettledLaunch, SettleFailure> {
    let summary = AppSummary {
        id: plan.manifest.id.clone(),
        name: plan.manifest.name.clone(),
        version: plan.manifest.version.clone(),
    };
    let assets = prepare_documents(&mut plan, dev_url)
        .await
        .map_err(SettleFailure::Documents)?;
    // The package is trusted before any right is decided: the trust question carries no rights.
    if let Some((signer, digest, inputs)) = trust {
        let asker = trust_asker.ok_or_else(|| {
            SettleFailure::Enforcement("trust inputs without a trust asker".to_owned())
        })?;
        decide_with_asker(
            &trust_folder(),
            &summary.id,
            &summary,
            signer
                .as_ref()
                .map(|(key, fingerprint)| (key.as_str(), fingerprint.as_str())),
            digest,
            inputs,
            asker,
        )
        .map_err(SettleFailure::Trust)?;
    }
    // The user decides what the application gets of what its manifest asks for; the store and
    // the window of the user are blocking work on a budget owned by this launch.
    let settle_executor =
        BlockingExecutor::new(NonZeroUsize::new(2).expect("non-zero consent settle budget"));
    let stored = stored_decisions(&settle_executor, decisions, identity)
        .await
        .map_err(SettleFailure::Store)?;
    let settled = settle_with_async(
        &settle_executor,
        &summary,
        &plan.permissions.rights(),
        &|right| plan.permissions.describe(right),
        stored,
        consent_asker,
    )
    .await
    .map_err(SettleFailure::Unanswered)?;
    if settled.changed {
        store_decisions(&settle_executor, decisions, identity, &settled.consent)
            .await
            .map_err(SettleFailure::Store)?;
    }
    let granted = Arc::new(
        (*plan.permissions)
            .clone()
            .with_consent(settled.consent.clone())
            .with_protected(&runtime_home()),
    );
    enforce(&granted, &settled.consent).map_err(SettleFailure::Enforcement)?;
    Ok(SettledLaunch {
        plan,
        assets,
        consent: settled.consent,
        granted,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        application::{
            plan::{make_package_plan, make_plan, package_path_vars, path_vars},
            source::{self, ApplicationSource},
            support,
        },
        consent::Asker,
    };
    use alef_core::security::{
        consent::{ConsentStore, Decision, Right},
        manifest::Manifest,
    };
    use std::{path::Path, time::Duration};
    use tokio::time::timeout;

    fn package_plan_with(
        dir: &Path,
        text: &str,
        files: &[(&str, String)],
    ) -> (Plan, Identity, String) {
        let mut files = files.to_vec();
        match files.iter().position(|(name, _)| *name == "alef.ktav") {
            Some(at) => files[at].1 = text.to_owned(),
            None => files.push(("alef.ktav", text.to_owned())),
        }
        let bytes = support::unsigned_package(
            &files
                .iter()
                .map(|(name, text)| (*name, text.as_bytes()))
                .collect::<Vec<_>>(),
        );
        std::fs::write(dir.join("app.alef"), &bytes).expect("write the package");
        match source::load(&dir.join("app.alef")).expect("a verified package") {
            ApplicationSource::Package(prepared) => {
                let prepared = *prepared;
                let identity = prepared.identity();
                let digest = prepared.archive_digest.clone();
                let vars =
                    package_path_vars(&prepared.manifest.id).expect("package path variables");
                let plan = make_package_plan(
                    prepared.package,
                    &prepared.selected_root,
                    prepared.manifest,
                    &vars,
                )
                .expect("a plan");
                (plan, identity, digest)
            }
            ApplicationSource::Directory { .. } => panic!("expected a package"),
        }
    }

    fn package_plan(dir: &Path, files: &[(&str, String)]) -> (Plan, Identity, String) {
        let text = files
            .iter()
            .find(|(name, _)| *name == "alef.ktav")
            .map(|(_, text)| text.clone())
            .unwrap_or_else(|| support::manifest_text(""));
        package_plan_with(dir, &text, files)
    }

    fn prepared_files() -> Vec<(&'static str, String)> {
        vec![
            ("alef.ktav", support::manifest_text("")),
            ("dist/index.html", "<html></html>".to_owned()),
        ]
    }

    #[tokio::test]
    async fn a_package_with_a_missing_entry_document_fails_before_any_trust_or_consent_question() {
        let dir = tempfile::tempdir().expect("tempdir");
        // The selected root exists, but the entry document the manifest window names does not.
        let mut files = prepared_files();
        files[1] = ("dist/other.html", "<html></html>".to_owned());
        let (plan, identity, digest) = package_plan(dir.path(), &files);
        let trust = (
            None,
            digest.clone(),
            TrustInputs {
                trust_key: None,
                allow_unsigned: false,
            },
        );
        let decisions = FileConsentStore::new(dir.path().join("consent"));
        let failure = timeout(
            Duration::from_secs(10),
            settle_launch(
                plan,
                &identity,
                Some(&trust),
                Some(&TrustAsker::Scripted(false)),
                &Asker::Nobody,
                &decisions,
                None,
            ),
        )
        .await
        .expect("settlement must complete within ten seconds")
        .err()
        .expect("the missing entry must fail");
        // `Scripted(false)` would refuse trust and `Nobody` would leave rights unanswered:
        // only the document check can have produced this failure.
        let SettleFailure::Documents(documents) = failure else {
            panic!("the documents must fail before any question: {failure:?}");
        };
        assert_eq!(documents.category, Some("MANIFEST_INVALID"));
        assert_eq!(documents.exit, 2);
        assert!(documents.detail.contains("index.html"), "{documents:?}");
    }

    #[tokio::test]
    async fn a_package_with_a_missing_window_document_also_fails_before_any_question() {
        let dir = tempfile::tempdir().expect("tempdir");
        let files = prepared_files();
        let (mut plan, identity, digest) = package_plan(dir.path(), &files);
        let mut second = plan.manifest.windows[0].clone();
        second.label = "tool".to_owned();
        second.url = "/gone.html".to_owned();
        plan.manifest.windows.push(second);
        let trust = (
            None,
            digest.clone(),
            TrustInputs {
                trust_key: None,
                allow_unsigned: false,
            },
        );
        let decisions = FileConsentStore::new(dir.path().join("consent"));
        let failure = timeout(
            Duration::from_secs(10),
            settle_launch(
                plan,
                &identity,
                Some(&trust),
                Some(&TrustAsker::Scripted(false)),
                &Asker::Nobody,
                &decisions,
                None,
            ),
        )
        .await
        .expect("settlement must complete within ten seconds")
        .err()
        .expect("the missing window document must fail");
        let SettleFailure::Documents(documents) = failure else {
            panic!("the documents must fail before any question: {failure:?}");
        };
        assert_eq!(documents.exit, 2);
        assert!(documents.detail.contains("gone.html"), "{documents:?}");
    }

    #[tokio::test]
    async fn a_package_with_present_documents_reaches_trust_and_consent_and_keeps_its_source() {
        let dir = tempfile::tempdir().expect("tempdir");
        let files = prepared_files();
        let (plan, identity, digest) = package_plan(dir.path(), &files);
        let trust = (
            None,
            digest.clone(),
            TrustInputs {
                trust_key: None,
                allow_unsigned: true,
            },
        );
        let decisions = FileConsentStore::new(dir.path().join("consent"));
        let settled = timeout(
            Duration::from_secs(10),
            settle_launch(
                plan,
                &identity,
                Some(&trust),
                Some(&TrustAsker::Scripted(false)),
                &Asker::Nobody,
                &decisions,
                None,
            ),
        )
        .await
        .expect("settlement must complete within ten seconds")
        .expect("present documents launch");
        // The built source is handed over; nothing rebuilds it at the bridge.
        assert!(settled.assets.is_some());
    }

    #[tokio::test]
    async fn an_unsigned_package_that_the_user_refuses_leaves_with_a_trust_refusal() {
        let dir = tempfile::tempdir().expect("tempdir");
        let files = prepared_files();
        let (plan, identity, digest) = package_plan(dir.path(), &files);
        let trust = (
            None,
            digest.clone(),
            TrustInputs {
                trust_key: None,
                allow_unsigned: false,
            },
        );
        let decisions = FileConsentStore::new(dir.path().join("consent"));
        let failure = timeout(
            Duration::from_secs(10),
            settle_launch(
                plan,
                &identity,
                Some(&trust),
                Some(&TrustAsker::Scripted(false)),
                &Asker::Nobody,
                &decisions,
                None,
            ),
        )
        .await
        .expect("settlement must complete within ten seconds")
        .err()
        .expect("a refused unsigned package must fail");
        let SettleFailure::Trust(TrustFailure::Refused(message)) = failure else {
            panic!("a refusal must be a trust refusal: {failure:?}");
        };
        assert!(!message.is_empty(), "{message}");
    }

    #[tokio::test]
    async fn the_documents_of_a_development_server_are_not_checked_before_the_launch() {
        let dir = tempfile::tempdir().expect("tempdir");
        let text = support::manifest_text("");
        std::fs::write(dir.path().join("alef.ktav"), &text).expect("write the manifest");
        std::fs::create_dir(dir.path().join("dist")).expect("the empty dist folder");
        let manifest = Manifest::from_ktav_str(&text).expect("a valid manifest");
        let vars = path_vars(dir.path(), &manifest.id).expect("path variables");
        let plan = make_plan(dir.path(), manifest, &vars).expect("a plan");
        let identity =
            crate::consent::identity_of(dir.path(), &plan.manifest.id).expect("an identity");
        let decisions = FileConsentStore::new(dir.path().join("consent"));
        let settled = timeout(
            Duration::from_secs(10),
            settle_launch(
                plan,
                &identity,
                None,
                None,
                &Asker::Nobody,
                &decisions,
                Some(&Url::parse("http://127.0.0.1:3000/").unwrap()),
            ),
        )
        .await
        .expect("settlement must complete within ten seconds")
        .expect("a development launch checks no documents");
        assert!(settled.assets.is_none());
    }

    #[tokio::test]
    async fn a_package_plan_keeps_its_snapshot_when_the_original_archive_changes() {
        let dir = tempfile::tempdir().expect("tempdir");
        let files = prepared_files();
        let (plan, identity, _) = package_plan(dir.path(), &files);
        std::fs::write(dir.path().join("app.alef"), b"not an archive anymore").unwrap();
        let decisions = FileConsentStore::new(dir.path().join("consent"));
        let settled = timeout(
            Duration::from_secs(10),
            settle_launch(
                plan,
                &identity,
                None,
                None,
                &Asker::Nobody,
                &decisions,
                None,
            ),
        )
        .await
        .expect("settlement must complete within ten seconds")
        .expect("the owned snapshot launches");
        assert!(settled.assets.is_some());
        assert_eq!(settled.plan.entry(), "/index.html");
    }

    #[tokio::test]
    async fn an_answered_consent_is_saved_into_the_store_of_the_application() {
        let dir = tempfile::tempdir().expect("tempdir");
        let text = support::manifest_text("").replace("read: []", "read: [ $APPDATA/data/* ]");
        let files: Vec<(&str, String)> = vec![("dist/index.html", "<html></html>".to_owned())];
        let (plan, identity, digest) = package_plan_with(dir.path(), &text, &files);
        let trust = (
            None,
            digest.clone(),
            TrustInputs {
                trust_key: None,
                allow_unsigned: true,
            },
        );
        let decisions = FileConsentStore::new(dir.path().join("consent"));
        timeout(
            Duration::from_secs(10),
            settle_launch(
                plan,
                &identity,
                Some(&trust),
                Some(&TrustAsker::Scripted(false)),
                &Asker::Grant(Decision::Allow),
                &decisions,
                None,
            ),
        )
        .await
        .expect("settlement must complete within ten seconds")
        .expect("a granted launch");
        let stored = decisions
            .load(&identity)
            .expect("the store reads back")
            .expect("the decision must be persisted");
        assert_eq!(
            stored
                .decisions()
                .map(|(right, decision)| (right.clone(), decision))
                .collect::<Vec<_>>(),
            vec![(Right::scoped("fs.read", "$APPDATA/data/*"), Decision::Allow)]
        );
    }

    #[tokio::test]
    async fn a_launch_settles_from_the_stored_decisions_without_asking_again() {
        let dir = tempfile::tempdir().expect("tempdir");
        let text = support::manifest_text("").replace("read: []", "read: [ $APPDATA/data/* ]");
        let files: Vec<(&str, String)> = vec![("dist/index.html", "<html></html>".to_owned())];
        let (plan, identity, _digest) = package_plan_with(dir.path(), &text, &files);
        let decisions = FileConsentStore::new(dir.path().join("consent"));
        let right = Right::scoped("fs.read", "$APPDATA/data/*");
        let mut stored = Consent::undecided();
        stored.set(right.clone(), Decision::Allow);
        decisions.save(&identity, &stored).expect("seed the store");
        let settled = timeout(
            Duration::from_secs(10),
            settle_launch(
                plan,
                &identity,
                None,
                None,
                &Asker::Nobody,
                &decisions,
                None,
            ),
        )
        .await
        .expect("settlement must complete within ten seconds")
        .expect("the stored decision answers the question");
        assert_eq!(settled.consent.decision(&right), Decision::Allow);
    }
}
