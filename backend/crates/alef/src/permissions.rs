// SPDX-License-Identifier: MIT OR Apache-2.0
//! `alef permissions`: what the user decided for an application, read and changed from the command
//! line. The decisions are the user's: this is the same store the start of an application asks.
use std::{fmt::Write as _, path::Path, sync::Arc};

use alef_core::security::{
    consent::{Consent, ConsentStore, Identity, Right},
    permissions::PermissionSet,
};

use crate::{
    application::{
        plan::{make_package_plan, package_path_vars},
        source,
    },
    args::PermissionsCommand,
    consent::{identity_of, word_of},
    plan::{load_manifest, make_plan, path_vars},
};

/// Who the application is and what its manifest asks for, a folder or a package.
fn asked_by(app_dir: &Path) -> Result<(Identity, Vec<Right>, Arc<PermissionSet>), String> {
    match source::load(app_dir) {
        Ok(source::ApplicationSource::Directory { dir }) => {
            let manifest = load_manifest(&dir).map_err(|e| e.message)?;
            let vars = path_vars(&dir, &manifest.id).map_err(|e| e.message)?;
            let id = manifest.id.clone();
            let plan = make_plan(&dir, manifest, &vars).map_err(|e| e.message)?;
            let rights = plan.permissions.rights();
            Ok((
                identity_of(&dir, &id).map_err(|e| e.message)?,
                rights,
                plan.permissions,
            ))
        }
        Ok(source::ApplicationSource::Package(prepared)) => {
            // The identity is computed before `package` and `manifest` are consumed.
            let identity = prepared.identity();
            let vars = package_path_vars(&prepared.manifest.id).map_err(|e| e.message)?;
            let plan = make_package_plan(
                prepared.package,
                &prepared.selected_root,
                prepared.manifest.clone(),
                &vars,
            )
            .map_err(|e| e.message)?;
            let rights = plan.permissions.rights();
            Ok((identity, rights, plan.permissions))
        }
        Err(failure) => Err(failure.to_string()),
    }
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
            let (identity, wanted, permissions) = asked_by(app)?;
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
                let _ = write!(out, "  {word}\t{right}");
                if let Some(detail) = permissions.describe(right) {
                    let _ = write!(out, "\t{detail}");
                }
                out.push('\n');
            }
        }
        PermissionsCommand::Set {
            app,
            right,
            decision,
        } => {
            let (identity, wanted, _) = asked_by(app)?;
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
            let (identity, _, _) = asked_by(app)?;
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
    use crate::consent::fixtures::{app, app_with_command};
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
    fn tray_is_enumerated_and_each_decision_is_stored_without_a_scope() {
        let scratch = tempfile::tempdir().unwrap();
        let store = FileConsentStore::new(scratch.path().join("store"));
        let folder = app(scratch.path(), "org.example.tray");
        let file = folder.join(crate::plan::MANIFEST_FILE);
        let text = std::fs::read_to_string(&file)
            .unwrap()
            .replace("tray: false", "tray: true");
        std::fs::write(file, text).unwrap();
        let list = PermissionsCommand::List {
            app: Some(folder.clone()),
        };
        assert!(run(&list, &store).unwrap().contains("  undecided\ttray\n"));
        for (decision, word) in [
            (Decision::Allow, "allow"),
            (Decision::Substitute, "substitute"),
            (Decision::Deny, "deny"),
        ] {
            let set = PermissionsCommand::Set {
                app: folder.clone(),
                right: "tray".parse().unwrap(),
                decision,
            };
            assert_eq!(run(&set, &store).unwrap(), format!("tray: {word}\n"));
            assert!(run(&list, &store)
                .unwrap()
                .contains(&format!("  {word}\ttray\n")));
        }
    }

    #[test]
    fn a_declared_command_is_listed_with_its_description_and_command_line() {
        let scratch = tempfile::tempdir().unwrap();
        let store = FileConsentStore::new(scratch.path().join("store"));
        let folder = app_with_command(scratch.path(), "org.example.one");
        let list = PermissionsCommand::List {
            app: Some(folder.clone()),
        };
        let listed = run(&list, &store).unwrap();
        assert!(
            listed.contains(
                "  undecided\tcli.command:status\tstatus — Shows the state of the folder (git status --short {path})\n"
            ),
            "{listed}"
        );
        assert!(
            listed.contains("  undecided\tcli.exec:sidecar:tool\n"),
            "{listed}"
        );
        let set = PermissionsCommand::Set {
            app: folder,
            right: "cli.command:status".parse().unwrap(),
            decision: Decision::Substitute,
        };
        run(&set, &store).unwrap();
        assert!(run(&list, &store)
            .unwrap()
            .contains("  substitute\tcli.command:status\tstatus — "));
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
    fn permissions_of_a_package_use_the_package_identity_and_never_touch_trust() {
        let scratch = tempfile::tempdir().unwrap();
        let store = FileConsentStore::new(scratch.path().join("store"));
        let (bytes, fingerprint) = crate::application::support::signed_package(&[
            (
                "alef.ktav",
                crate::application::support::manifest_text("").as_bytes(),
            ),
            ("dist/index.html", b"<html></html>"),
        ]);
        let package = scratch.path().join("app.alef");
        std::fs::write(&package, &bytes).unwrap();
        let list = PermissionsCommand::List {
            app: Some(package.clone()),
        };
        let listed = run(&list, &store).unwrap();
        assert!(listed.contains("org.example.app"), "{listed}");
        assert!(listed.contains("undecided\tapp.env:HOME"), "{listed}");
        let set = PermissionsCommand::Set {
            app: package.clone(),
            right: "clipboard.read".parse().unwrap(),
            decision: Decision::Substitute,
        };
        assert_eq!(run(&set, &store).unwrap(), "clipboard.read: substitute\n");
        assert!(run(&list, &store)
            .unwrap()
            .contains("substitute\tclipboard.read"));
        let all = run(&PermissionsCommand::List { app: None }, &store).unwrap();
        assert!(
            all.contains(&format!("org.example.app\tpkg-signed:{fingerprint}\n")),
            "{all}"
        );
        let reset = PermissionsCommand::Reset { app: package };
        assert!(run(&reset, &store).unwrap().contains("asks again"));
        // No trust pin was created or replaced: the pin path next to a scratch trust folder
        // stays absent (the real `trust_folder` depends on process-global state).
        assert!(!crate::application::trust::pin_path(
            &scratch.path().join("trust"),
            "org.example.app"
        )
        .exists());
        // The decisions were stored under the package identity, which the source loader agrees on.
        let prepared =
            match crate::application::source::load(&scratch.path().join("app.alef")).unwrap() {
                crate::application::source::ApplicationSource::Package(prepared) => *prepared,
                crate::application::source::ApplicationSource::Directory { .. } => {
                    panic!("expected a package")
                }
            };
        assert_eq!(
            prepared.identity(),
            Identity {
                app_id: "org.example.app".to_owned(),
                signer: Some(format!("pkg-signed:{fingerprint}")),
            }
        );
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
