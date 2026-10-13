// SPDX-License-Identifier: MIT OR Apache-2.0
//! Parsing and terminal reports for local runtime management, without launch side effects.
use std::{ffi::OsString, fmt::Write as _, path::Path};

use crate::{
    args::RuntimeCommand,
    runtimes::{install, prune, store, StoreError},
};

fn invalid(detail: impl std::fmt::Display) -> String {
    format!("INVALID_ARGUMENT: {detail}")
}

fn required(
    arguments: &mut impl Iterator<Item = OsString>,
    name: &str,
) -> Result<OsString, String> {
    let value = arguments
        .next()
        .ok_or_else(|| invalid(format!("runtime: {name} is required")))?;
    if value.is_empty() || value.as_encoded_bytes().starts_with(b"-") {
        return Err(invalid(format!(
            "runtime: {name} is required, not {value:?}"
        )));
    }
    Ok(value)
}

fn version(value: OsString) -> Result<String, String> {
    let text = value
        .into_string()
        .map_err(|_| invalid("runtime: version is not valid text"))?;
    if !store::is_plain_semver(&text) {
        return Err(invalid(format!(
            "{text:?} is not a strict MAJOR.MINOR.PATCH version"
        )));
    }
    Ok(text)
}

/// Parses arguments after the leading `runtime`; all errors carry a usage category.
pub fn parse(arguments: impl IntoIterator<Item = OsString>) -> Result<RuntimeCommand, String> {
    let mut arguments = arguments.into_iter();
    let verb = required(&mut arguments, "list, install or prune")?
        .into_string()
        .map_err(|_| invalid("runtime: verb is not valid text"))?;
    let command = match verb.as_str() {
        "list" => RuntimeCommand::List,
        "install" => RuntimeCommand::Install {
            version: version(required(&mut arguments, "<VERSION>")?)?,
            source_binary: required(&mut arguments, "<SOURCE_BINARY>")?.into(),
        },
        "prune" => {
            let mut keep = Vec::new();
            let mut dry_run = false;
            parse_prune_options(&mut arguments, &mut keep, &mut dry_run)?;
            RuntimeCommand::Prune { keep, dry_run }
        }
        other => return Err(invalid(format!("runtime: unknown verb {other:?}"))),
    };
    if let Some(extra) = arguments.next() {
        return Err(invalid(format!(
            "runtime {verb}: unexpected argument {extra:?}"
        )));
    }
    Ok(command)
}

fn parse_prune_options(
    arguments: &mut impl Iterator<Item = OsString>,
    keep: &mut Vec<String>,
    dry_run: &mut bool,
) -> Result<(), String> {
    while let Some(flag) = arguments.next() {
        match flag.to_str() {
            Some("--keep") => keep.push(version(required(arguments, "--keep <VERSION>")?)?),
            Some("--dry-run") if !*dry_run => *dry_run = true,
            Some("--dry-run") => return Err(invalid("runtime prune: --dry-run is given twice")),
            _ => return Err(invalid(format!("runtime prune: unknown argument {flag:?}"))),
        }
    }
    Ok(())
}

/// Executes a runtime command against an explicit store and returns its terminal report.
pub fn run_in(command: &RuntimeCommand, root: &Path) -> Result<String, StoreError> {
    let mut output = String::new();
    match command {
        RuntimeCommand::List => {
            for installed in store::installed_in(root).map_err(StoreError::Store)? {
                let marker = if installed.version == env!("CARGO_PKG_VERSION") {
                    " *"
                } else {
                    ""
                };
                let _ = writeln!(
                    output,
                    "{}{marker}\t{}",
                    installed.version,
                    installed.binary.display()
                );
            }
        }
        RuntimeCommand::Install {
            version,
            source_binary,
        } => {
            let binary = install::install_in(root, version, source_binary)?;
            let _ = writeln!(output, "{version}\t{}", binary.display());
        }
        RuntimeCommand::Prune { keep, dry_run } => {
            let report = prune::prune_in(root, keep, *dry_run)?;
            for version in report.removed {
                let _ = writeln!(output, "removed\t{version}");
            }
            for version in report.would_remove {
                let _ = writeln!(output, "would-remove\t{version}");
            }
            for (version, reason) in report.skipped {
                let _ = writeln!(output, "skipped\t{version}\t{reason}");
            }
        }
    }
    Ok(output)
}

/// Executes a runtime command against the launcher's default store.
pub fn run(command: &RuntimeCommand) -> Result<String, StoreError> {
    run_in(command, &store::root())
}

/// Explains incompatibility using one ordered store snapshot, without relaunching.
pub fn incompatible_detail_in(root: &Path, requirement: &str) -> String {
    let prefix = format!(
        "requires {requirement}; this executable provides {}",
        env!("CARGO_PKG_VERSION")
    );
    let installed = match store::installed_in(root) {
        Ok(installed) => installed,
        Err(error) => return format!("{prefix}; installed runtimes: unreadable at {}: {error}; runtime relaunch is not implemented in this slice", root.display()),
    };
    if let Some(selected) = installed.iter().rev().find(|entry| {
        alef_core::security::manifest::runtime_requirement_met(requirement, &entry.version)
    }) {
        return format!("{prefix}; selected installed runtime: {} at {}; runtime relaunch is not implemented in this slice", selected.version, selected.binary.display());
    }
    let versions = if installed.is_empty() {
        "none".to_owned()
    } else {
        installed
            .iter()
            .map(|entry| entry.version.as_str())
            .collect::<Vec<_>>()
            .join(", ")
    };
    format!("{prefix}; installed runtimes: {versions}; no installed runtime satisfies {requirement}; install a matching local binary with alef runtime install <version> <path-to-alef-binary>; runtime relaunch is not implemented in this slice")
}

/// Explains incompatibility against the launcher's default store.
pub fn incompatible_detail(requirement: &str) -> String {
    incompatible_detail_in(&store::root(), requirement)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        args::{parse_args, Command},
        runtimes::support::Scratch,
    };
    use std::path::PathBuf;

    fn cli(words: &[&str]) -> Result<Command, String> {
        parse_args(words.iter().map(OsString::from))
    }

    #[test]
    fn runtime_commands_are_parsed_before_launch_options_and_keep_may_repeat() {
        assert_eq!(
            cli(&["runtime", "list"]).unwrap(),
            Command::Runtime(RuntimeCommand::List)
        );
        assert_eq!(
            cli(&["runtime", "install", "1.2.3", "binary"]).unwrap(),
            Command::Runtime(RuntimeCommand::Install {
                version: "1.2.3".to_owned(),
                source_binary: PathBuf::from("binary"),
            })
        );
        assert_eq!(
            cli(&[
                "runtime",
                "prune",
                "--keep",
                "1.0.0",
                "--dry-run",
                "--keep",
                "2.0.0",
                "--keep",
                "1.0.0"
            ])
            .unwrap(),
            Command::Runtime(RuntimeCommand::Prune {
                keep: vec!["1.0.0".to_owned(), "2.0.0".to_owned(), "1.0.0".to_owned()],
                dry_run: true,
            })
        );
        assert_eq!(
            cli(&["runtime", "prune"]).unwrap(),
            Command::Runtime(RuntimeCommand::Prune {
                keep: Vec::new(),
                dry_run: false
            })
        );
    }

    #[test]
    fn every_runtime_argument_error_has_the_invalid_argument_prefix() {
        for words in [
            &["runtime"][..],
            &["runtime", "unknown"],
            &["runtime", "list", "extra"],
            &["runtime", "list", "--help"],
            &["runtime", "install"],
            &["runtime", "install", "1.0.0"],
            &["runtime", "install", "1.0.0", "--dry-run"],
            &["runtime", "install", "1.0.0", ""],
            &["runtime", "install", "1.0.0", "binary", "extra"],
            &["runtime", "prune", "--unknown"],
            &["runtime", "prune", "extra"],
            &["runtime", "prune", "--keep"],
            &["runtime", "prune", "--keep", "--dry-run"],
            &["runtime", "prune", "--dry-run", "--dry-run"],
        ] {
            let error = cli(words).unwrap_err();
            assert!(
                error.starts_with("INVALID_ARGUMENT: "),
                "{words:?}: {error}"
            );
        }
        for bad in [
            "",
            "1.0",
            "1.0.0-beta",
            "1.0.0+build",
            "../1.0.0",
            "1.0.0/../2.0.0",
            "01.0.0",
        ] {
            for words in [
                vec!["runtime", "install", bad, "binary"],
                vec!["runtime", "prune", "--keep", bad],
            ] {
                assert!(
                    cli(&words).unwrap_err().starts_with("INVALID_ARGUMENT: "),
                    "{words:?}"
                );
            }
        }
    }

    #[cfg(any(unix, windows))]
    fn nontext() -> OsString {
        #[cfg(unix)]
        {
            use std::os::unix::ffi::OsStringExt;
            OsString::from_vec(vec![b'b', 0xff])
        }
        #[cfg(windows)]
        {
            use std::os::windows::ffi::OsStringExt;
            OsString::from_wide(&[0x62, 0xd800])
        }
    }

    #[cfg(any(unix, windows))]
    #[test]
    fn a_nonunicode_binary_path_is_preserved_but_nontext_versions_and_flags_are_refused() {
        let path = nontext();
        let command = parse_args([
            OsString::from("runtime"),
            OsString::from("install"),
            OsString::from("1.0.0"),
            path.clone(),
        ])
        .unwrap();
        assert_eq!(
            command,
            Command::Runtime(RuntimeCommand::Install {
                version: "1.0.0".to_owned(),
                source_binary: PathBuf::from(path)
            })
        );
        for words in [
            vec![OsString::from("runtime"), nontext()],
            vec![
                OsString::from("runtime"),
                OsString::from("install"),
                nontext(),
                OsString::from("binary"),
            ],
            vec![
                OsString::from("runtime"),
                OsString::from("prune"),
                OsString::from("--keep"),
                nontext(),
            ],
            vec![
                OsString::from("runtime"),
                OsString::from("prune"),
                nontext(),
            ],
        ] {
            assert!(parse_args(words)
                .unwrap_err()
                .starts_with("INVALID_ARGUMENT: "));
        }
    }

    fn place(root: &Path, version: &str) {
        let folder = root.join(version);
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join(store::BINARY_NAME), b"binary").unwrap();
    }

    #[test]
    fn incompatibility_with_an_empty_store_names_the_local_install_command() {
        let scratch = Scratch::new();
        let detail = incompatible_detail_in(scratch.path(), "^1");
        assert!(detail.contains("installed runtimes: none"));
        assert!(detail.contains("alef runtime install <version> <path-to-alef-binary>"));
    }

    #[test]
    fn incompatible_installed_versions_are_reported_in_numeric_order_with_an_install_hint() {
        let scratch = Scratch::new();
        for version in ["1.10.0", "1.2.0", "1.0.0"] {
            place(scratch.path(), version);
        }
        let detail = incompatible_detail_in(scratch.path(), "^2");
        assert!(detail.contains("installed runtimes: 1.0.0, 1.2.0, 1.10.0"));
        assert!(detail.contains("alef runtime install <version> <path-to-alef-binary>"));
    }

    #[test]
    fn a_fitting_installed_runtime_names_the_highest_binary_without_claiming_install_is_needed() {
        let scratch = Scratch::new();
        for version in ["1.2.0", "2.0.0", "1.10.0"] {
            place(scratch.path(), version);
        }
        let detail = incompatible_detail_in(scratch.path(), "^1");
        assert!(detail.contains(&format!(
            "selected installed runtime: 1.10.0 at {}",
            scratch
                .path()
                .join("1.10.0")
                .join(store::BINARY_NAME)
                .display()
        )));
        assert!(detail.contains("runtime relaunch is not implemented"));
        assert!(!detail.contains("alef runtime install"));
        assert!(!detail.contains("no installed runtime satisfies"));
    }

    #[test]
    fn an_unreadable_store_is_reported_as_unreadable_not_empty_or_install_needed() {
        let scratch = Scratch::new();
        let root = scratch.path().join("store-file");
        std::fs::write(&root, b"not a directory").unwrap();
        let detail = incompatible_detail_in(&root, "^1");
        assert!(detail.contains(&format!(
            "installed runtimes: unreadable at {}:",
            root.display()
        )));
        assert!(!detail.contains("installed runtimes: none"));
        assert!(!detail.contains("alef runtime install"));
    }

    #[test]
    fn listing_marks_only_the_running_installed_version() {
        let scratch = Scratch::new();
        let root = scratch.path();
        let running = env!("CARGO_PKG_VERSION");
        place(root, running);
        place(root, "999.0.0");
        let output = run_in(&RuntimeCommand::List, root).unwrap();
        assert_eq!(
            output,
            format!(
                "{running} *\t{}\n999.0.0\t{}\n",
                root.join(running).join(store::BINARY_NAME).display(),
                root.join("999.0.0").join(store::BINARY_NAME).display()
            )
        );
    }

    #[test]
    fn install_copies_the_source_bytes_and_reports_the_published_path() {
        let scratch = Scratch::new();
        let root = scratch.path().join("store");
        let source = scratch.path().join("source");
        std::fs::write(&source, b"local runtime bytes").unwrap();
        let output = run_in(
            &RuntimeCommand::Install {
                version: "1.0.0".to_owned(),
                source_binary: source,
            },
            &root,
        )
        .unwrap();
        let binary = root.join("1.0.0").join(store::BINARY_NAME);
        assert_eq!(std::fs::read(&binary).unwrap(), b"local runtime bytes");
        assert_eq!(output, format!("1.0.0\t{}\n", binary.display()));
    }

    #[test]
    fn prune_reports_a_dry_run_without_removing_and_then_reports_real_removal() {
        let scratch = Scratch::new();
        let root = scratch.path();
        place(root, "2.0.0");
        let dry = RuntimeCommand::Prune {
            keep: Vec::new(),
            dry_run: true,
        };
        assert_eq!(run_in(&dry, root).unwrap(), "would-remove\t2.0.0\n");
        assert!(root.join("2.0.0").join(store::BINARY_NAME).is_file());
        let real = RuntimeCommand::Prune {
            keep: Vec::new(),
            dry_run: false,
        };
        assert_eq!(run_in(&real, root).unwrap(), "removed\t2.0.0\n");
        assert!(!root.join("2.0.0").exists());
    }
}
