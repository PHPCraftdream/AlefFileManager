// SPDX-License-Identifier: MIT OR Apache-2.0
//! The shared tail of the launch pipeline: the entry and window documents are checked through
//! the same lookup that serves requests, then trust and consent are settled. The check runs
//! before any question is asked or answer written.
use std::sync::Arc;

use alef_core::security::{
    consent::{Consent, ConsentStore, FileConsentStore, Identity},
    permissions::PermissionSet,
};
use alef_runtime::{prepare_assets, PreparedAssets};
use url::Url;

use super::{
    plan::Plan,
    source::LoadFailure,
    trust::{decide_with_asker, trust_folder, TrustAsker, TrustFailure, TrustInputs},
};
use crate::consent::{ask::AppSummary, enforce, runtime_home, settle_with, Unanswered};

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

/// Checks the documents, then settles trust and consent, in exactly this order.
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
    // The documents first: one lookup, the one requests use, before trust or consent writes.
    let assets = if dev_url.is_some() {
        None
    } else {
        let entry = plan.entry();
        let documents: Vec<String> = plan
            .manifest
            .windows
            .iter()
            .map(|window| window.url.clone())
            .collect();
        Some(
            prepare_assets(plan.take_assets(), Some(&entry), &documents)
                .await
                .map_err(|error| {
                    SettleFailure::Documents(LoadFailure {
                        category: Some("MANIFEST_INVALID"),
                        detail: format!("assets: {error}"),
                        exit: 2,
                    })
                })?,
        )
    };
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
    // The user decides what the application gets of what its manifest asks for.
    let stored = decisions.load(identity).map_err(SettleFailure::Store)?;
    let settled = settle_with(
        &summary,
        &plan.permissions.rights(),
        &|right| plan.permissions.describe(right),
        stored,
        consent_asker,
    )
    .map_err(SettleFailure::Unanswered)?;
    if settled.changed {
        decisions
            .save(identity, &settled.consent)
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
            plan::{make_package_plan, package_path_vars},
            source::{self, ApplicationSource},
            support,
        },
        consent::Asker,
    };
    use std::path::Path;

    fn package_plan(dir: &Path, files: &[(&str, String)]) -> (Plan, source::Prepared) {
        let bytes = support::unsigned_package(
            &files
                .iter()
                .map(|(name, text)| (*name, text.as_bytes()))
                .collect::<Vec<_>>(),
        );
        std::fs::write(dir.join("app.alef"), &bytes).expect("write the package");
        match source::load(&dir.join("app.alef")).expect("a verified package") {
            ApplicationSource::Package(mut prepared) => {
                let prepared = *prepared;
                let vars =
                    package_path_vars(&prepared.manifest.id).expect("package path variables");
                let plan = make_package_plan(
                    prepared.package,
                    &prepared.selected_root,
                    prepared.manifest,
                    &vars,
                )
                .expect("a plan");
                (plan, prepared)
            }
            ApplicationSource::Directory { .. } => panic!("expected a package"),
        }
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
        let (plan, prepared) = package_plan(dir.path(), &files);
        let identity = prepared.identity();
        let trust = (
            None,
            prepared.archive_digest.clone(),
            TrustInputs {
                trust_key: None,
                allow_unsigned: false,
            },
        );
        let decisions = FileConsentStore::new(dir.path().join("consent"));
        let failure = settle_launch(
            plan,
            &identity,
            Some(&trust),
            Some(&TrustAsker::Scripted(false)),
            &Asker::Nobody,
            &decisions,
            None,
        )
        .await
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
        let manifest = support::manifest_text("").replace("url: /index.html", "url: /gone.html");
        let files = vec![
            ("alef.ktav", manifest),
            ("dist/index.html", "<html></html>".to_owned()),
        ];
        let (plan, prepared) = package_plan(dir.path(), &files);
        let identity = prepared.identity();
        let trust = (
            None,
            prepared.archive_digest.clone(),
            TrustInputs {
                trust_key: None,
                allow_unsigned: false,
            },
        );
        let decisions = FileConsentStore::new(dir.path().join("consent"));
        let failure = settle_launch(
            plan,
            &identity,
            Some(&trust),
            Some(&TrustAsker::Scripted(false)),
            &Asker::Nobody,
            &decisions,
            None,
        )
        .await
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
        let (plan, prepared) = package_plan(dir.path(), &files);
        let identity = prepared.identity();
        let trust = (
            None,
            prepared.archive_digest.clone(),
            TrustInputs {
                trust_key: None,
                allow_unsigned: true,
            },
        );
        let decisions = FileConsentStore::new(dir.path().join("consent"));
        let settled = settle_launch(
            plan,
            &identity,
            Some(&trust),
            Some(&TrustAsker::Scripted(false)),
            &Asker::Nobody,
            &decisions,
            None,
        )
        .await
        .expect("present documents launch");
        // The built source is handed over; nothing rebuilds it at the bridge.
        assert!(settled.assets.is_some());
    }
}
