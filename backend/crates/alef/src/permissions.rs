// SPDX-License-Identifier: MIT OR Apache-2.0
//! `alef permissions`: what the user decided for an application, read and changed from the command
//! line. The decisions are the user's: this is the same store the start of an application asks.
use std::{fmt::Write as _, path::Path};

use alef_core::{
    error::AlefError,
    security::consent::{Consent, ConsentStore, Identity, Right},
};

use crate::{
    args::PermissionsCommand,
    consent::{identity_of, word_of},
    plan::{load_manifest, make_plan, path_vars},
};

/// Who the folder is and what its manifest asks for.
fn asked_by(app_dir: &Path) -> Result<(Identity, Vec<Right>), AlefError> {
    let manifest = load_manifest(app_dir)?;
    let vars = path_vars(app_dir, &manifest.id)?;
    let id = manifest.id.clone();
    let plan = make_plan(app_dir, manifest, &vars)?;
    Ok((identity_of(app_dir, &id)?, plan.permissions.rights()))
}

fn io(error: std::io::Error) -> String {
    format!("the decisions cannot be read or written: {error}")
}

/// Does what the command says and reports it as text for the terminal.
pub fn run(command: &PermissionsCommand, store: &dyn ConsentStore) -> Result<String, String> {
    let mut out = String::new();
    match command {
        PermissionsCommand::List { app: None } => {
            let identities = store.identities().map_err(io)?;
            if identities.is_empty() {
                out.push_str("no decisions yet\n");
            }
            for identity in identities {
                let place = identity.signer.as_deref().unwrap_or("-");
                let _ = writeln!(out, "{}\t{place}", identity.app_id);
            }
        }
        PermissionsCommand::List { app: Some(app) } => {
            let (identity, wanted) = asked_by(app).map_err(|e| e.message)?;
            let consent = store.load(&identity).map_err(io)?;
            let _ = writeln!(out, "{}", identity.app_id);
            if wanted.is_empty() {
                out.push_str("  asks for nothing\n");
            }
            for right in &wanted {
                let word = match &consent {
                    Some(consent) if consent.decided(right) => word_of(consent.decision(right)),
                    _ => "undecided",
                };
                let _ = writeln!(out, "  {word}\t{right}");
            }
        }
        PermissionsCommand::Set {
            app,
            right,
            decision,
        } => {
            let (identity, wanted) = asked_by(app).map_err(|e| e.message)?;
            if !wanted.contains(right) {
                return Err(format!(
                    "{} does not ask for {right}; it asks for: {}",
                    identity.app_id,
                    wanted
                        .iter()
                        .map(ToString::to_string)
                        .collect::<Vec<_>>()
                        .join(", ")
                ));
            }
            let mut consent = store
                .load(&identity)
                .map_err(io)?
                .unwrap_or_else(Consent::undecided);
            consent.set(right.clone(), *decision);
            store.save(&identity, &consent).map_err(io)?;
            let _ = writeln!(out, "{right}: {}", word_of(*decision));
        }
        PermissionsCommand::Reset { app } => {
            let (identity, _) = asked_by(app).map_err(|e| e.message)?;
            store.forget(&identity).map_err(io)?;
            let _ = writeln!(
                out,
                "forgotten: the next start of {} asks again",
                identity.app_id
            );
        }
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::consent::fixtures::app;
    use alef_core::security::consent::{Decision, FileConsentStore};

    #[test]
    fn the_user_lists_sets_and_forgets_the_decisions_for_an_application() {
        let scratch = tempfile::tempdir().unwrap();
        let store = FileConsentStore::new(scratch.path().join("store"));
        let folder = app(scratch.path(), "org.example.one");
        let list = PermissionsCommand::List {
            app: Some(folder.clone()),
        };
        assert_eq!(
            run(&list, &store).unwrap(),
            "org.example.one\n  undecided\tapp.env:HOME\n  undecided\tclipboard.read\n"
        );
        assert_eq!(
            run(&PermissionsCommand::List { app: None }, &store).unwrap(),
            "no decisions yet\n"
        );
        let set = |right: &str, decision| PermissionsCommand::Set {
            app: folder.clone(),
            right: right.parse().unwrap(),
            decision,
        };
        assert_eq!(
            run(&set("clipboard.read", Decision::Substitute), &store).unwrap(),
            "clipboard.read: substitute\n"
        );
        run(&set("app.env:HOME", Decision::Deny), &store).unwrap();
        assert_eq!(
            run(&list, &store).unwrap(),
            "org.example.one\n  deny\tapp.env:HOME\n  substitute\tclipboard.read\n"
        );
        let all = run(&PermissionsCommand::List { app: None }, &store).unwrap();
        assert!(all.starts_with("org.example.one\tdir:"), "{all}");

        let reset = PermissionsCommand::Reset {
            app: folder.clone(),
        };
        assert!(run(&reset, &store).unwrap().contains("asks again"));
        assert!(run(&list, &store)
            .unwrap()
            .contains("undecided\tapp.env:HOME"));
    }

    #[test]
    fn a_right_the_manifest_does_not_ask_for_is_refused_and_the_asked_ones_are_named() {
        let scratch = tempfile::tempdir().unwrap();
        let store = FileConsentStore::new(scratch.path().join("store"));
        let folder = app(scratch.path(), "org.example.one");
        let error = run(
            &PermissionsCommand::Set {
                app: folder,
                right: "secrets".parse().unwrap(),
                decision: Decision::Allow,
            },
            &store,
        )
        .unwrap_err();
        assert!(error.contains("does not ask for secrets"), "{error}");
        assert!(error.contains("app.env:HOME, clipboard.read"), "{error}");
        assert!(
            store.identities().unwrap().is_empty(),
            "nothing was written"
        );
    }

    #[test]
    fn two_folders_with_one_id_have_decisions_of_their_own() {
        let scratch = tempfile::tempdir().unwrap();
        let store = FileConsentStore::new(scratch.path().join("store"));
        let first = app(&scratch.path().join("a"), "org.example.one");
        let second = app(&scratch.path().join("b"), "org.example.one");
        run(
            &PermissionsCommand::Set {
                app: first.clone(),
                right: "clipboard.read".parse().unwrap(),
                decision: Decision::Allow,
            },
            &store,
        )
        .unwrap();
        let listed = |folder: &Path| {
            run(
                &PermissionsCommand::List {
                    app: Some(folder.to_owned()),
                },
                &store,
            )
            .unwrap()
        };
        assert!(listed(&first).contains("allow\tclipboard.read"));
        assert!(listed(&second).contains("undecided\tclipboard.read"));
    }

    #[test]
    fn a_folder_without_a_manifest_is_reported() {
        let scratch = tempfile::tempdir().unwrap();
        let store = FileConsentStore::new(scratch.path().join("store"));
        let error = run(
            &PermissionsCommand::Reset {
                app: scratch.path().to_owned(),
            },
            &store,
        )
        .unwrap_err();
        assert!(error.contains("alef.ktav"), "{error}");
    }
}
