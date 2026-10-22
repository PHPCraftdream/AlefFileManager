// SPDX-License-Identifier: MIT OR Apache-2.0
//! `alef permissions` and `alef ai`: what the user decided for an application and what its AI may
//! do per day, read and changed from the command line. The decisions are the user's: this is the
//! same store the start of an application asks.
use std::{
    ffi::OsString,
    fmt::Write as _,
    path::{Path, PathBuf},
    sync::Arc,
};

use alef_core::security::{
    consent::{ConsentStore, Identity, Right},
    permissions::PermissionSet,
};
use alef_core::session::usage;

use crate::{
    application::{
        plan::{make_package_plan, package_path_vars},
        source,
    },
    args::{AiCommand, Command, PermissionsCommand},
    consent::{ai_folder, decision_named, identity_of, word_of},
    plan::{load_manifest, make_plan, path_vars},
};

/// Who the application is and what its manifest asks for, a folder or a package.
fn asked_by(app_dir: &Path) -> Result<(Identity, Vec<Right>, Arc<PermissionSet>), String> {
    match source::load_metadata(app_dir) {
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

fn text_of(argument: OsString, what: &str) -> Result<String, String> {
    argument
        .into_string()
        .map_err(|_| format!("{what} is not valid text"))
}

fn folder(arguments: &mut impl Iterator<Item = OsString>, verb: &str) -> Result<PathBuf, String> {
    arguments
        .next()
        .map(PathBuf::from)
        .ok_or_else(|| format!("permissions {verb}: <DIRECTORY> is required"))
}

/// Parses the arguments after the program name.
pub(crate) fn parse_permissions(
    mut arguments: impl Iterator<Item = OsString>,
) -> Result<Command, String> {
    let verb = arguments
        .next()
        .and_then(|verb| verb.into_string().ok())
        .ok_or_else(|| "permissions: list, set or reset?".to_owned())?;
    let command = match verb.as_str() {
        "list" => PermissionsCommand::List {
            app: arguments.next().map(PathBuf::from),
        },
        "set" => {
            let app = folder(&mut arguments, "set")?;
            let right = text_of(
                arguments
                    .next()
                    .ok_or("permissions set: <RIGHT> is required")?,
                "the right",
            )?;
            let decision = text_of(
                arguments
                    .next()
                    .ok_or("permissions set: <DECISION> is required")?,
                "the decision",
            )?;
            let right = right.parse()?;
            let decision = decision_named(&decision)?;
            let confirm_red = match arguments.next() {
                None => false,
                Some(flag) if flag == "--confirm-red" => true,
                Some(extra) => {
                    return Err(format!(
                        "permissions set: unexpected argument {}",
                        extra.to_string_lossy()
                    ))
                }
            };
            PermissionsCommand::Set {
                app,
                right,
                decision,
                confirm_red,
            }
        }
        "reset" => PermissionsCommand::Reset {
            app: folder(&mut arguments, "reset")?,
        },
        other => return Err(format!("permissions: unknown verb {other:?}")),
    };
    if let Some(extra) = arguments.next() {
        return Err(format!(
            "permissions {verb}: unexpected argument {}",
            extra.to_string_lossy()
        ));
    }
    Ok(Command::Permissions(command))
}

/// The `<DIRECTORY>` positional of `alef ai`, named after its verb on failure.
fn app_of(arguments: &mut impl Iterator<Item = OsString>, verb: &str) -> Result<PathBuf, String> {
    arguments
        .next()
        .map(PathBuf::from)
        .ok_or_else(|| format!("ai {verb}: <DIRECTORY> is required"))
}

/// Parses the arguments of `alef ai` after the program name.
pub(crate) fn parse_ai(mut arguments: impl Iterator<Item = OsString>) -> Result<Command, String> {
    let verb = arguments
        .next()
        .and_then(|verb| verb.into_string().ok())
        .ok_or_else(|| "ai: limits or log?".to_owned())?;
    let command = match verb.as_str() {
        "limits" => {
            let app = app_of(&mut arguments, "limits")?;
            let (mut requests, mut tokens) = (None, None);
            while let Some(flag) = arguments.next() {
                let name = flag.to_str().unwrap_or("").to_owned();
                let slot = match name.as_str() {
                    "--requests" => &mut requests,
                    "--tokens" => &mut tokens,
                    other => return Err(format!("ai limits: unknown flag {other}")),
                };
                if slot.is_some() {
                    return Err(format!("ai limits: {name} is given twice"));
                }
                let text = arguments
                    .next()
                    .ok_or_else(|| format!("ai limits: {name} requires a value"))?;
                let text = text
                    .into_string()
                    .map_err(|_| format!("ai limits: {name} is not valid text"))?;
                *slot = Some(
                    text.parse::<u64>()
                        .map_err(|_| format!("ai limits: {name} expects a number, got {text}"))?,
                );
            }
            AiCommand::Limits {
                app,
                requests,
                tokens,
            }
        }
        "log" => AiCommand::Log {
            app: app_of(&mut arguments, "log")?,
        },
        other => return Err(format!("ai: unknown verb {other:?}")),
    };
    if let Some(extra) = arguments.next() {
        return Err(format!(
            "ai {verb}: unexpected argument {}",
            extra.to_string_lossy()
        ));
    }
    Ok(Command::Ai(command))
}

/// The word for a limit: the number, or `no limit` when the user set none.
fn limit_word(bound: Option<u64>) -> String {
    bound.map_or_else(|| "no limit".to_owned(), |n| n.to_string())
}

/// Does what the command says and reports it as text for the terminal.
pub fn run_ai(command: &AiCommand) -> Result<String, String> {
    let app = match command {
        AiCommand::Limits { app, .. } | AiCommand::Log { app } => app,
    };
    let (identity, _, _) = asked_by(app)?;
    let folder = ai_folder(&identity);
    let store = usage::Store::new(folder.clone());
    let mut out = String::new();
    match command {
        AiCommand::Limits {
            requests, tokens, ..
        } => {
            let mut limits = store.limits().map_err(io)?;
            if requests.is_some() || tokens.is_some() {
                if requests.is_some() {
                    limits.requests = *requests;
                }
                if tokens.is_some() {
                    limits.tokens = *tokens;
                }
                store.set_limits(&limits).map_err(io)?;
            }
            let _ = writeln!(out, "{}", identity.app_id);
            let _ = writeln!(out, "  requests  {}", limit_word(limits.requests));
            let _ = writeln!(out, "  tokens    {}", limit_word(limits.tokens));
        }
        AiCommand::Log { .. } => {
            let lines = usage::Journal::read(&folder).map_err(io)?;
            if lines.is_empty() {
                out.push_str("no AI activity recorded\n");
            }
            for line in &lines {
                let Ok(entry) = serde_json::from_str::<usage::Entry>(line) else {
                    out.push_str("  (damaged entry)\n");
                    continue;
                };
                let kind = match entry.kind {
                    usage::Kind::Tool => "tool",
                    usage::Kind::Builtin => "builtin",
                };
                let event = match entry.event {
                    usage::Event::Start => "start".to_owned(),
                    usage::Event::End => format!("end {}", entry.outcome.as_deref().unwrap_or("?")),
                };
                let _ = writeln!(
                    out,
                    "{}  {}  {}  {} {}  {}",
                    usage::timestamp(entry.at),
                    entry.app,
                    entry.role,
                    kind,
                    entry.name,
                    event
                );
            }
        }
    }
    Ok(out)
}

/// Does what the command says and reports it as text for the terminal.
pub fn run(command: &PermissionsCommand, store: &dyn ConsentStore) -> Result<String, String> {
    run_in(command, store, &crate::consent::runtime_home())
}

/// Runs permissions against an explicit runtime home for deterministic callers and tests.
pub fn run_in(
    command: &PermissionsCommand,
    store: &dyn ConsentStore,
    home: &Path,
) -> Result<String, String> {
    use crate::apps::AppsIndex;
    let mut out = String::new();
    match command {
        PermissionsCommand::List { app: None } => {
            for identity in store.identities().map_err(io)? {
                if store
                    .load(&identity)
                    .map_err(io)?
                    .is_some_and(|consent| consent.decisions().next().is_some())
                {
                    let place = identity.signer.as_deref().unwrap_or("-");
                    let _ = writeln!(out, "{}\t{place}", identity.app_id);
                }
            }
            if out.is_empty() {
                out.push_str("no decisions yet\n");
            }
        }
        PermissionsCommand::List { app: Some(app) } => {
            let description = AppsIndex::new(home, store).describe(app)?;
            let _ = writeln!(out, "{}", description.identity.app_id);
            if description.rights.is_empty() {
                out.push_str("  asks for nothing\n");
            }
            for right in description.rights {
                let word = right.decision.map(word_of).unwrap_or("undecided");
                let _ = write!(out, "  {word}\t{}", right.right);
                if right.red {
                    out.push_str("\tRED");
                    if let Some(risk) = right.right.risk() {
                        let _ = write!(out, "\t{risk}");
                    }
                }
                if let Some(detail) = right.description {
                    let _ = write!(out, "\t{detail}");
                }
                out.push('\n');
            }
        }
        PermissionsCommand::Set {
            app,
            right,
            decision,
            confirm_red,
        } => {
            AppsIndex::new(home, store).set(app, right, *decision, *confirm_red)?;
            let _ = writeln!(out, "{right}: {}", word_of(*decision));
        }
        PermissionsCommand::Reset { app } => {
            let identity = AppsIndex::new(home, store).reset(app)?;
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

    /// Serializes the tests that move `ALEF_HOME`: the process env is global.
    static AI_HOME_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());

    /// Holds the lock and the scratch folder, and restores `ALEF_HOME` on drop — also on panic.
    struct AiHomeGuard<'a> {
        _lock: std::sync::MutexGuard<'a, ()>,
        _scratch: tempfile::TempDir,
        previous: Option<std::ffi::OsString>,
    }

    impl AiHomeGuard<'_> {
        fn take() -> Self {
            // Poisoned lock is fine: it still serializes.
            let _lock = AI_HOME_LOCK.lock().unwrap_or_else(|e| e.into_inner());
            let scratch = tempfile::tempdir().unwrap();
            let previous = std::env::var_os(crate::consent::HOME_VARIABLE);
            std::env::set_var(crate::consent::HOME_VARIABLE, scratch.path());
            Self {
                _lock,
                _scratch: scratch,
                previous,
            }
        }
    }

    impl Drop for AiHomeGuard<'_> {
        fn drop(&mut self) {
            match self.previous.take() {
                Some(value) => std::env::set_var(crate::consent::HOME_VARIABLE, value),
                None => std::env::remove_var(crate::consent::HOME_VARIABLE),
            }
        }
    }

    /// Runs the body with `ALEF_HOME` pointed at a scratch folder, so `ai_folder` of the identity
    /// lands there; the env is restored afterwards, even when the body panics.
    fn with_ai_home<T>(body: impl FnOnce() -> T) -> T {
        let _guard = AiHomeGuard::take();
        body()
    }

    #[test]
    fn alef_ai_limits_sets_and_shows_the_daily_quota() {
        with_ai_home(|| {
            let scratch = tempfile::tempdir().unwrap();
            let folder = app(scratch.path(), "org.example.quota");
            let set = AiCommand::Limits {
                app: folder.clone(),
                requests: Some(5),
                tokens: Some(1000),
            };
            let out = run_ai(&set).unwrap();
            assert!(out.contains("org.example.quota"), "{out}");
            assert!(out.contains("requests"), "{out}");
            assert!(out.contains('5'), "{out}");
            assert!(out.contains("1000"), "{out}");
            // Without flags the stored values are shown again.
            let shown = run_ai(&AiCommand::Limits {
                app: folder.clone(),
                requests: None,
                tokens: None,
            })
            .unwrap();
            assert_eq!(shown, out);
            let identity = identity_of(&folder, "org.example.quota").unwrap();
            let limits = usage::Store::new(ai_folder(&identity)).limits().unwrap();
            assert_eq!(limits.requests, Some(5));
            assert_eq!(limits.tokens, Some(1000));
        });
    }

    #[test]
    fn alef_ai_log_prints_what_the_application_did() {
        with_ai_home(|| {
            let scratch = tempfile::tempdir().unwrap();
            let folder = app(scratch.path(), "org.example.log");
            let identity = identity_of(&folder, "org.example.log").unwrap();
            let journal = usage::Journal::new(ai_folder(&identity));
            let entry = |event, outcome| usage::Entry {
                at: usage::now_ms(),
                app: "org.example.log".to_owned(),
                role: "smart".to_owned(),
                kind: usage::Kind::Tool,
                name: "summarizeSelection".to_owned(),
                event,
                outcome,
            };
            journal.append(&entry(usage::Event::Start, None)).unwrap();
            journal
                .append(&entry(usage::Event::End, Some("ok".to_owned())))
                .unwrap();
            let out = run_ai(&AiCommand::Log { app: folder }).unwrap();
            assert!(out.contains("org.example.log"), "{out}");
            assert!(out.contains("smart"), "{out}");
            assert!(out.contains("tool summarizeSelection"), "{out}");
            assert!(out.contains("start"), "{out}");
            assert!(out.contains("ok"), "{out}");
            // A fresh application has no journal yet.
            let fresh = app(scratch.path(), "org.example.fresh");
            let out = run_ai(&AiCommand::Log { app: fresh }).unwrap();
            assert_eq!(out, "no AI activity recorded\n");
        });
    }

    #[test]
    fn alef_ai_rejects_unknown_verbs_and_flags() {
        fn parse_ai_args(arguments: &[&str]) -> Result<Command, String> {
            parse_ai(arguments.iter().map(OsString::from))
        }
        assert!(parse_ai_args(&["limits", "site", "--wat", "1"]).is_err());
        assert!(parse_ai_args(&["history", "site"]).is_err());
        assert!(
            parse_ai_args(&["limits", "site", "--requests", "1", "--requests", "2"]).is_err(),
            "a flag is given twice"
        );
        assert!(parse_ai_args(&["limits", "site", "--requests", "x"]).is_err());
        assert!(matches!(
            parse_ai_args(&["limits", "site", "--requests", "0"]),
            Ok(Command::Ai(AiCommand::Limits {
                requests: Some(0),
                ..
            }))
        ));
    }

    #[test]
    fn parser_handles_confirm_red_once() {
        assert!(parse_permissions(
            ["set", "site", "tray", "allow", "--confirm-red"]
                .map(OsString::from)
                .into_iter()
        )
        .is_ok());
        let error = parse_permissions(
            [
                "set",
                "site",
                "tray",
                "allow",
                "--confirm-red",
                "--confirm-red",
            ]
            .map(OsString::from)
            .into_iter(),
        )
        .unwrap_err();
        assert!(error.contains("unexpected argument --confirm-red"));
    }

    #[test]
    fn red_cli_set_requires_confirmation() {
        let scratch = tempfile::tempdir().unwrap();
        let store = FileConsentStore::new(scratch.path().join("store"));
        let folder = app_with_command(scratch.path(), "org.example.red");
        let denied = PermissionsCommand::Set {
            app: folder.clone(),
            right: "cli.exec:sidecar:tool".parse().unwrap(),
            decision: Decision::Allow,
            confirm_red: false,
        };
        assert!(run(&denied, &store).unwrap_err().contains("--confirm-red"));
        let confirmed = PermissionsCommand::Set {
            app: folder,
            right: "cli.exec:sidecar:tool".parse().unwrap(),
            decision: Decision::Allow,
            confirm_red: true,
        };
        assert!(run(&confirmed, &store).is_ok());
    }

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
            confirm_red: false,
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
                confirm_red: false,
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
        assert!(!listed
            .lines()
            .filter(|line| line.contains("cli.command"))
            .any(|line| line.contains("RED")));
        assert!(
            listed.contains(
                "  undecided\tcli.exec:sidecar:tool\tRED\tbypasses all protections, including the AI model keys\n"
            ),
            "{listed}"
        );
        let set = PermissionsCommand::Set {
            app: folder,
            right: "cli.command:status".parse().unwrap(),
            decision: Decision::Substitute,
            confirm_red: false,
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
                confirm_red: false,
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
                confirm_red: false,
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
        let manifest = crate::application::support::manifest_text("")
            .replace("read: false", "read: true")
            .replace("env: []", "env: [ HOME ]");
        let (bytes, fingerprint) = crate::application::support::signed_package(&[
            ("alef.ktav", manifest.as_bytes()),
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
            confirm_red: false,
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
    fn builtin_execution_is_listed_as_red_but_other_builtin_rights_are_not() {
        let scratch = tempfile::tempdir().unwrap();
        let store = FileConsentStore::new(scratch.path().join("store"));
        let folder = app(scratch.path(), "org.example.builtin");
        let file = folder.join(crate::plan::MANIFEST_FILE);
        let text = std::fs::read_to_string(&file).unwrap().replace(
            "    app: {",
            "    ai: {\n        roles: [smart]\n        tools: []\n        builtin: [\n            {\n                name: exec\n            }\n        ]\n    }\n    app: {",
        );
        std::fs::write(file, text).unwrap();

        let listed = run(&PermissionsCommand::List { app: Some(folder) }, &store).unwrap();
        assert!(listed
            .lines()
            .filter(|line| line.contains("ai.builtin.exec"))
            .all(|line| line
                .contains("\tRED\tbypasses all protections, including the AI model keys")));
        assert!(listed
            .lines()
            .filter(|line| !line.contains("ai.builtin.exec") && !line.contains("cli.exec"))
            .all(|line| !line.contains("\tRED")));
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
