// SPDX-License-Identifier: MIT OR Apache-2.0
//! Command line of `alef`.
use std::{ffi::OsString, path::PathBuf};

use alef_core::security::consent::{Decision, Right};
use url::Url;

use crate::consent::decision_named;

pub const USAGE: &str =
    "alef run <PATH|ID> [launcher options] [-- <APPLICATION ARGUMENTS>...]\n\
alef --app <DIRECTORY> [launcher options] [-- <APPLICATION ARGUMENTS>...]\n\
alef permissions list [<DIRECTORY>]\n\
alef permissions set <DIRECTORY> <RIGHT> <DECISION>\n\
alef permissions reset <DIRECTORY>\n\
alef ai limits <DIRECTORY> [--requests <N>] [--tokens <N>]\n\
alef ai log <DIRECTORY>\n\
alef trust <REQUEST> <ANSWER>\n\
alef runtime list\n\
alef runtime install <VERSION> <SOURCE_BINARY>\n\
alef runtime prune [--keep <VERSION>]... [--dry-run]\nalef install <PACKAGE> [--trust-key <KEY>] [--allow-unsigned]\nalef uninstall <ID> [--purge-data]\nalef apps [--json]\n\
`run` and `--app` are the same launch: a directory runs as a development application,\n\
a file is an Alef package; when the argument is neither an existing file nor a directory,\n\
it is the id of an application installed by `alef install` (`alef apps` lists them);\n\
an existing file or directory always wins. Runs it: its alef.ktav manifest and assets, in embedded Servo.\n\
  --app DIRECTORY   application directory containing alef.ktav\n\
  --dev-url URL     load the document from a development server on 127.0.0.1 instead of the files\n\
  --grant DECISION  decide every right the application asks for and you have not decided on yet:\n\
                    allow, substitute (a stand-in the application cannot tell from the real thing)\n\
                    or deny; the decision is remembered\n\
  --trust-key KEY   the 64 lowercase hex fingerprint of the signer of a package: the\n\
                    noninteractive equivalent of accepting the trust question\n\
  --allow-unsigned  run an unsigned package for this launch without asking\n\
  --no-prompt       never show the permission window: an application with rights you have not\n\
                    decided on does not start\n\
  --                everything after it is the command line of the application\n\
                    (parsed by the `arguments` of its manifest; `-- --help` prints its usage)\n\
permissions: what you decided for an application. RIGHT is `permission` or `permission:scope`\n\
  (clipboard.read, app.env:HOME, fs.read:$DOCUMENTS/**); `list` without DIRECTORY lists the applications\n\
  you have decided for, `reset` makes the next start ask again\n\
ai: the daily AI quota of an application and what its AI did (the journal); N of 0 means blocked\n\
trust: answer a trust request\n\
runtime: manage installed local runtime versions; install copies a local binary, no download\n\
  VERSION is strict MAJOR.MINOR.PATCH; prune always keeps the running version";

/// A validated launch request.
#[derive(Debug, PartialEq, Eq)]
pub struct Launch {
    pub target: PathBuf,
    pub dev_url: Option<Url>,
    /// The command line of the application: trailing URLs and everything after `--`.
    pub app_args: Vec<OsString>,
    /// `--grant`: the decision for every right not decided yet.
    pub grant: Option<Decision>,
    /// `--no-prompt`: the permission window is never shown.
    pub no_prompt: bool,
    /// `--trust-key`: the fingerprint the user vouches for; only a package accepts it.
    pub trust_key: Option<String>,
    /// `--allow-unsigned`: run an unsigned package without asking.
    pub allow_unsigned: bool,
    /// Internal window-child selection; never an application option.
    pub launch_window: bool,
    /// The target came from the positional `run <ARG>` form: the only form where an
    /// installed manifest id may be resolved.
    pub positional: bool,
}

impl Launch {
    /// Options that belong to the other source are refused with a usage error (exit code 2).
    pub fn check(&self, package: bool) -> Result<(), String> {
        if package {
            if self.dev_url.is_some() {
                return Err(
                    "--dev-url is not accepted for a package: the manifest and its assets are the only source"
                        .to_owned(),
                );
            }
        } else {
            if self.trust_key.is_some() {
                return Err("--trust-key is only accepted for a package".to_owned());
            }
            if self.allow_unsigned {
                return Err("--allow-unsigned is only accepted for a package".to_owned());
            }
        }
        Ok(())
    }
}

/// `alef permissions ...`: what the user decided for an application.
#[derive(Debug, PartialEq, Eq)]
pub enum PermissionsCommand {
    /// The decisions for the application in the folder, or the applications that have any.
    List { app: Option<PathBuf> },
    Set {
        app: PathBuf,
        right: Right,
        decision: Decision,
    },
    /// Forgets the decisions: the next start asks again.
    Reset { app: PathBuf },
}

/// `alef ai ...`: the daily AI quota the user sets for an application and its journal.
#[derive(Debug, PartialEq, Eq)]
pub enum AiCommand {
    /// Shows, and with a flag sets, the daily limits of the application.
    Limits {
        app: PathBuf,
        requests: Option<u64>,
        tokens: Option<u64>,
    },
    /// Prints the AI journal of the application.
    Log { app: PathBuf },
}

/// `alef runtime ...`: local runtime version management.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RuntimeCommand {
    List,
    Install {
        version: String,
        source_binary: PathBuf,
    },
    Prune {
        keep: Vec<String>,
        dry_run: bool,
    },
}

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Run(Launch),
    Permissions(PermissionsCommand),
    Ai(AiCommand),
    Runtime(RuntimeCommand),
    Install {
        package: PathBuf,
        trust_key: Option<String>,
        allow_unsigned: bool,
    },
    Uninstall {
        id: String,
        purge_data: bool,
    },
    Apps {
        json: bool,
    },
    /// `alef consent <REQUEST> <ANSWER>`, started by the launcher itself: the permission window.
    Consent {
        request: PathBuf,
        answer: PathBuf,
    },
    /// `alef trust <REQUEST> <ANSWER>`, started by the launcher itself: the trust window.
    Trust {
        request: PathBuf,
        answer: PathBuf,
    },
    Help,
}

fn value(arguments: &mut impl Iterator<Item = OsString>, flag: &str) -> Result<OsString, String> {
    arguments
        .next()
        .ok_or_else(|| format!("{flag} requires a value"))
}

fn dev_url(text: &str) -> Result<Url, String> {
    let url = Url::parse(text).map_err(|error| format!("--dev-url is not a URL: {error}"))?;
    if url.scheme() != "http"
        || url.host_str() != Some("127.0.0.1")
        || !url.username().is_empty()
        || url.password().is_some()
    {
        return Err("--dev-url must be an http URL on 127.0.0.1 without credentials".to_owned());
    }
    Ok(url)
}

fn text_of(argument: OsString, what: &str) -> Result<String, String> {
    argument
        .into_string()
        .map_err(|_| format!("{what} is not valid text"))
}

/// Strict parser for the install-management commands; malformed forms are usage errors.
fn parse_install(mut args: impl Iterator<Item = OsString>) -> Result<Command, String> {
    let verb = args
        .next()
        .and_then(|s| s.into_string().ok())
        .ok_or("install command is required")?;
    let command = match verb.as_str() {
        "install" => {
            let package = args
                .next()
                .filter(|path| !path.as_encoded_bytes().starts_with(b"-"))
                .map(PathBuf::from)
                .ok_or("install: <PACKAGE> is required")?;
            let (mut key, mut unsigned) = (None, false);
            while let Some(arg) = args.next() {
                match arg.to_str() {
                    Some("--trust-key") if key.is_none() => {
                        key = Some(text_of(value(&mut args, "--trust-key")?, "trust key")?)
                    }
                    Some("--trust-key") => return Err("--trust-key is given twice".into()),
                    Some("--allow-unsigned") if !unsigned => unsigned = true,
                    Some("--allow-unsigned") => {
                        return Err("--allow-unsigned is given twice".into())
                    }
                    _ => {
                        return Err(format!(
                            "install: unknown argument {}",
                            arg.to_string_lossy()
                        ))
                    }
                }
            }
            Command::Install {
                package,
                trust_key: key,
                allow_unsigned: unsigned,
            }
        }
        "uninstall" => {
            let id = args
                .next()
                .ok_or_else(|| "uninstall: <ID> is required".to_string())
                .and_then(|s| text_of(s, "id"))?;
            let purge_data = match args.next() {
                None => false,
                Some(s) if s == "--purge-data" => true,
                Some(s) => {
                    return Err(format!(
                        "uninstall: unknown argument {}",
                        s.to_string_lossy()
                    ))
                }
            };
            if args.next().is_some() {
                return Err("uninstall: unexpected argument".into());
            }
            Command::Uninstall { id, purge_data }
        }
        "apps" => {
            let json = match args.next() {
                None => false,
                Some(s) if s == "--json" => true,
                Some(s) => return Err(format!("apps: unknown argument {}", s.to_string_lossy())),
            };
            if args.next().is_some() {
                return Err("apps: unexpected argument".into());
            }
            Command::Apps { json }
        }
        _ => return Err(format!("unknown install command {verb:?}")),
    };
    Ok(command)
}

/// Parses the arguments after the program name.
pub fn parse_args(arguments: impl IntoIterator<Item = OsString>) -> Result<Command, String> {
    let mut arguments = arguments.into_iter().peekable();
    if arguments
        .peek()
        .is_some_and(|first| matches!(first.to_str(), Some("install" | "uninstall" | "apps")))
    {
        return parse_install(arguments).map_err(|e| format!("INVALID_ARGUMENT: {e}"));
    }
    if arguments
        .peek()
        .is_some_and(|first| first.to_str() == Some("runtime"))
    {
        arguments.next();
        return crate::runtimes::command::parse(arguments).map(Command::Runtime);
    }
    if arguments
        .peek()
        .is_some_and(|first| first.to_str() == Some("permissions"))
    {
        arguments.next();
        return crate::consent::permissions::parse_permissions(arguments);
    }
    if arguments
        .peek()
        .is_some_and(|first| first.to_str() == Some("ai"))
    {
        arguments.next();
        return crate::consent::permissions::parse_ai(arguments);
    }
    let mut after_run = false;
    if arguments
        .peek()
        .is_some_and(|first| first.to_str() == Some("run"))
    {
        arguments.next();
        after_run = true;
    }
    if !after_run
        && arguments
            .peek()
            .is_some_and(|first| first.to_str() == Some("consent"))
    {
        arguments.next();
        let request = arguments.next().ok_or("consent: <REQUEST> is required")?;
        let answer = arguments.next().ok_or("consent: <ANSWER> is required")?;
        return match arguments.next() {
            None => Ok(Command::Consent {
                request: PathBuf::from(request),
                answer: PathBuf::from(answer),
            }),
            Some(extra) => Err(format!(
                "consent: unexpected argument {}",
                extra.to_string_lossy()
            )),
        };
    }
    if !after_run
        && arguments
            .peek()
            .is_some_and(|first| first.to_str() == Some("trust"))
    {
        arguments.next();
        let request = arguments.next().ok_or("trust: <REQUEST> is required")?;
        let answer = arguments.next().ok_or("trust: <ANSWER> is required")?;
        return match arguments.next() {
            None => Ok(Command::Trust {
                request: PathBuf::from(request),
                answer: PathBuf::from(answer),
            }),
            Some(extra) => Err(format!(
                "trust: unexpected argument {}",
                extra.to_string_lossy()
            )),
        };
    }
    let mut target = None;
    let mut dev = None;
    let mut grant = None;
    let mut trust_key = None;
    let mut allow_unsigned = false;
    let mut no_prompt = false;
    let mut launch_window = false;
    let mut positional = false;
    let mut app_args = Vec::new();
    while let Some(argument) = arguments.next() {
        match argument.to_str() {
            Some("--") => {
                app_args.extend(arguments.by_ref());
                break;
            }
            // A link can carry a quote that breaks out of its place in the command line: what
            // comes after a URL is never an option of the runtime.
            Some(
                flag @ ("--app" | "--dev-url" | "--grant" | "--no-prompt" | "--launch-mode"
                | "--trust-key" | "--allow-unsigned"),
            ) if !app_args.is_empty() => {
                return Err(format!("{flag} is not accepted after a URL"));
            }
            Some("--app") if target.is_some() => return Err("--app is given twice".to_owned()),
            Some("--dev-url") if dev.is_some() => return Err("--dev-url is given twice".to_owned()),
            Some("--grant") if grant.is_some() => return Err("--grant is given twice".to_owned()),
            Some("--app") => target = Some(PathBuf::from(value(&mut arguments, "--app")?)),
            Some("--dev-url") => {
                let text = value(&mut arguments, "--dev-url")?;
                dev = Some(dev_url(&text.to_string_lossy())?);
            }
            Some("--grant") => {
                let word = text_of(value(&mut arguments, "--grant")?, "--grant")?;
                grant = Some(decision_named(&word)?);
            }
            Some("--launch-mode") if launch_window => {
                return Err("--launch-mode is given twice".to_owned())
            }
            Some("--launch-mode") => {
                let word = text_of(value(&mut arguments, "--launch-mode")?, "--launch-mode")?;
                if word != "window" {
                    return Err("--launch-mode must be window".to_owned());
                }
                launch_window = true;
            }
            Some("--trust-key") if trust_key.is_some() => {
                return Err("--trust-key is given twice".to_owned())
            }
            Some("--trust-key") => {
                trust_key = Some(text_of(
                    value(&mut arguments, "--trust-key")?,
                    "--trust-key",
                )?);
            }
            Some("--allow-unsigned") if allow_unsigned => {
                return Err("--allow-unsigned is given twice".to_owned())
            }
            Some("--allow-unsigned") => allow_unsigned = true,
            Some("--no-prompt") => no_prompt = true,
            Some("--help" | "-h") => return Ok(Command::Help),
            Some(text)
                if !text.starts_with('-')
                    && !text.chars().any(char::is_control)
                    // A one-letter scheme is a Windows drive (`C:\app.alef`), not a link.
                    && Url::parse(text).is_ok_and(|url| url.scheme().len() > 1) =>
            {
                app_args.push(argument)
            }
            Some(text) if after_run && !text.starts_with('-') => {
                if target.is_some() {
                    return Err("the application is given twice".to_owned());
                }
                positional = true;
                target = Some(PathBuf::from(argument));
            }
            _ if argument.as_encoded_bytes().starts_with(b"-") => {
                return Err(format!("unknown argument: {}", argument.to_string_lossy()))
            }
            _ => {
                if !after_run {
                    return Err(format!("unknown argument: {}", argument.to_string_lossy()));
                }
                if target.is_some() {
                    return Err("the application is given twice".to_owned());
                }
                positional = true;
                target = Some(PathBuf::from(argument));
            }
        }
    }
    let target = target.ok_or_else(|| "--app <PATH> is required".to_owned())?;
    Ok(Command::Run(Launch {
        target,
        dev_url: dev,
        app_args,
        grant,
        trust_key,
        allow_unsigned,
        no_prompt,
        launch_window,
        positional,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(arguments: &[&str]) -> Result<Command, String> {
        parse_args(arguments.iter().map(OsString::from))
    }

    #[test]
    fn an_absolute_windows_package_path_is_the_target_and_not_a_link() {
        let Command::Run(launch) =
            parse(&["run", r"C:\Temp\app.alef", "--trust-key", &"a".repeat(64)]).unwrap()
        else {
            panic!("run")
        };
        assert_eq!(launch.target, PathBuf::from(r"C:\Temp\app.alef"));
        assert!(launch.app_args.is_empty());
        assert!(launch.trust_key.is_some());
    }

    #[test]
    fn the_hidden_window_mode_is_strict_and_only_belongs_to_the_runtime_before_the_separator() {
        let Command::Run(run) = parse(&["--launch-mode", "window", "--app", "site"]).unwrap()
        else {
            panic!("run")
        };
        assert!(run.launch_window);
        assert!(!USAGE.contains("--launch-mode"));
        for bad in [
            "cli",
            "foreground",
            "undecided",
            "Window",
            "",
            "window ",
            "--",
        ] {
            assert!(
                parse(&["--app", "site", "--launch-mode", bad]).is_err(),
                "{bad:?}"
            );
        }
        assert!(parse(&["--app", "site", "--launch-mode"])
            .unwrap_err()
            .contains("requires a value"));
        let Command::Run(run) = parse(&["--app", "site", "--", "--launch-mode", "window"]).unwrap()
        else {
            panic!("run")
        };
        assert!(!run.launch_window);
        assert_eq!(
            run.app_args,
            [OsString::from("--launch-mode"), OsString::from("window")]
        );
    }

    #[test]
    fn trailing_urls_belong_to_the_application_but_unknown_options_and_strays_do_not() {
        let Command::Run(launch) =
            parse(&["--app", "site", "alef://open", "--", "alef:next"]).unwrap()
        else {
            panic!("run");
        };
        assert_eq!(
            launch.app_args,
            [OsString::from("alef://open"), OsString::from("alef:next")]
        );
        for bad in ["stray", "--unknown", "alef://[bad", "alef:bad\n"] {
            assert!(parse(&["--app", "site", bad]).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn no_option_of_the_runtime_follows_a_url_or_comes_twice() {
        for after in [
            ["--app", "other"].as_slice(),
            &["--grant", "allow"],
            &["--dev-url", "http://127.0.0.1:3000/"],
            &["--no-prompt"],
            &["--launch-mode", "window"],
            &["--trust-key", "a"],
            &["--allow-unsigned"],
        ] {
            let mut line = vec!["--app", "site", "alef://open"];
            line.extend(after);
            assert!(parse(&line).is_err(), "{line:?}");
        }
        for twice in [
            ["--app", "a", "--app", "b"].as_slice(),
            &["--app", "a", "--grant", "allow", "--grant", "deny"],
            &[
                "--app",
                "a",
                "--launch-mode",
                "window",
                "--launch-mode",
                "window",
            ],
            &[
                "--app",
                "a",
                "--dev-url",
                "http://127.0.0.1:1/",
                "--dev-url",
                "http://127.0.0.1:2/",
            ],
        ] {
            assert!(parse(twice).is_err(), "{twice:?}");
        }
        // What the registration of a scheme passes: everything after the double dash is the application's.
        let Command::Run(launch) =
            parse(&["--app", "site", "--", "alef:x", "--grant", "allow"]).unwrap()
        else {
            panic!("run");
        };
        assert!(launch.grant.is_none());
        assert_eq!(launch.app_args.len(), 3);
    }

    #[test]
    fn app_and_dev_url_are_read() {
        let command = parse(&["--app", "site", "--dev-url", "http://127.0.0.1:3000/"]).unwrap();
        assert_eq!(
            command,
            Command::Run(Launch {
                target: PathBuf::from("site"),
                dev_url: Some(Url::parse("http://127.0.0.1:3000/").unwrap()),
                app_args: Vec::new(),
                grant: None,
                trust_key: None,
                allow_unsigned: false,
                no_prompt: false,
                launch_window: false,
                positional: false,
            })
        );
        assert_eq!(
            parse(&["--app", "site"]).unwrap(),
            Command::Run(Launch {
                target: PathBuf::from("site"),
                dev_url: None,
                app_args: Vec::new(),
                grant: None,
                trust_key: None,
                allow_unsigned: false,
                no_prompt: false,
                launch_window: false,
                positional: false,
            })
        );
    }

    #[test]
    fn everything_after_the_double_dash_belongs_to_the_application() {
        let command = parse(&["--app", "site", "--", "--help", "--app", "x", "-h", "--"]).unwrap();
        let Command::Run(launch) = command else {
            panic!("a run, not the launcher help");
        };
        assert_eq!(launch.target, PathBuf::from("site"));
        let expected: Vec<OsString> = ["--help", "--app", "x", "-h", "--"]
            .iter()
            .map(OsString::from)
            .collect();
        assert_eq!(launch.app_args, expected);
        assert!(parse(&["--", "--app", "x"]).unwrap_err().contains("--app"));
    }

    #[test]
    fn help_wins_and_everything_else_is_strict() {
        assert_eq!(parse(&["--help"]).unwrap(), Command::Help);
        assert_eq!(parse(&["--app", "x", "-h"]).unwrap(), Command::Help);
        assert!(parse(&[]).unwrap_err().contains("--app"));
        assert!(parse(&["--app"]).unwrap_err().contains("requires a value"));
        assert!(parse(&["--dev-url"])
            .unwrap_err()
            .contains("requires a value"));
        assert!(parse(&["--app", "x", "--frontend-dir", "y"])
            .unwrap_err()
            .contains("unknown argument"));
        assert!(parse(&["stray"]).unwrap_err().contains("unknown argument"));
    }

    #[test]
    fn install_management_commands_are_strict_and_preserve_paths() {
        assert_eq!(
            parse(&[
                "install",
                "packages/app.alef",
                "--trust-key",
                "fingerprint",
                "--allow-unsigned"
            ])
            .unwrap(),
            Command::Install {
                package: PathBuf::from("packages/app.alef"),
                trust_key: Some("fingerprint".to_owned()),
                allow_unsigned: true,
            }
        );
        assert_eq!(
            parse(&["uninstall", "app-id", "--purge-data"]).unwrap(),
            Command::Uninstall {
                id: "app-id".to_owned(),
                purge_data: true
            }
        );
        assert_eq!(
            parse(&["apps", "--json"]).unwrap(),
            Command::Apps { json: true }
        );

        for bad in [
            &["install"][..],
            &["install", "--allow-unsigned"],
            &["install", "pkg", "--allow-unsigned", "--allow-unsigned"],
            &["install", "pkg", "--trust-key", "a", "--trust-key", "b"],
            &["install", "pkg", "extra"],
            &["apps", "--json", "extra"],
            &["apps", "--unknown"],
            &["uninstall"],
            &["uninstall", "id", "--purge-data", "--purge-data"],
            &["uninstall", "id", "extra"],
        ] {
            assert!(
                parse(bad).unwrap_err().contains("INVALID_ARGUMENT"),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn apps_default_and_install_unsigned_parse() {
        assert_eq!(
            parse(&["install", "folder/app.alef", "--allow-unsigned"]).unwrap(),
            Command::Install {
                package: PathBuf::from("folder/app.alef"),
                trust_key: None,
                allow_unsigned: true
            }
        );
        assert_eq!(parse(&["apps"]).unwrap(), Command::Apps { json: false });
        assert!(parse(&["install", "--trust-key"])
            .unwrap_err()
            .contains("INVALID_ARGUMENT"));
    }

    #[test]
    fn a_multibyte_target_is_kept_as_an_os_string() {
        let Command::Run(launch) = parse(&["run", "приложение"]).unwrap() else {
            panic!("a run");
        };
        assert_eq!(launch.target, PathBuf::from("приложение"));
    }

    #[test]
    fn run_and_app_are_the_same_launch_and_a_positional_after_run_is_the_target() {
        for (line, expected) in [
            (&["--app", "site"][..], "site"),
            (&["run", "site"], "site"),
            (&["run", "site.alef"], "site.alef"),
        ] {
            let Command::Run(launch) = parse(line).unwrap() else {
                panic!("a run for {line:?}");
            };
            assert_eq!(launch.target, PathBuf::from(expected), "{line:?}");
        }
        assert!(parse(&["stray"]).unwrap_err().contains("unknown argument"));
        assert!(parse(&["run", "a", "b"])
            .unwrap_err()
            .contains("the application is given twice"));
        assert!(parse(&["run", "a", "--app", "b"])
            .unwrap_err()
            .contains("is given twice"));
    }

    #[test]
    fn trust_flags_are_captured_and_refused_after_a_url() {
        let Command::Run(launch) = parse(&[
            "run",
            "site.alef",
            "--trust-key",
            "aabb",
            "--allow-unsigned",
        ])
        .unwrap() else {
            panic!("a run");
        };
        assert_eq!(launch.trust_key.as_deref(), Some("aabb"));
        assert!(launch.allow_unsigned);
        assert!(
            parse(&["run", "site.alef", "--trust-key", "a", "--trust-key", "b"])
                .unwrap_err()
                .contains("--trust-key is given twice")
        );
        assert!(
            parse(&["run", "site.alef", "--allow-unsigned", "--allow-unsigned"])
                .unwrap_err()
                .contains("--allow-unsigned is given twice")
        );
        for after in [&["--trust-key", "a"][..], &["--allow-unsigned"]] {
            let mut line = vec!["run", "site.alef", "alef://open"];
            line.extend(after);
            assert!(parse(&line)
                .unwrap_err()
                .contains("is not accepted after a URL"));
        }
    }

    #[test]
    fn options_of_the_other_source_are_refused_by_check() {
        let mut launch = Launch {
            target: PathBuf::from("site.alef"),
            dev_url: Some(Url::parse("http://127.0.0.1:3000/").unwrap()),
            app_args: Vec::new(),
            grant: None,
            trust_key: Some("aabb".to_owned()),
            allow_unsigned: true,
            no_prompt: false,
            launch_window: false,
            positional: false,
        };
        assert!(launch
            .check(true)
            .unwrap_err()
            .contains("--dev-url is not accepted for a package"));
        launch.dev_url = None;
        assert_eq!(launch.check(true), Ok(()));
        assert!(launch
            .check(false)
            .unwrap_err()
            .contains("--trust-key is only accepted for a package"));
        launch.trust_key = None;
        assert!(launch
            .check(false)
            .unwrap_err()
            .contains("--allow-unsigned is only accepted for a package"));
        launch.allow_unsigned = false;
        assert_eq!(launch.check(false), Ok(()));
    }

    #[test]
    fn the_trust_subcommand_takes_exactly_a_request_and_an_answer() {
        assert_eq!(
            parse(&["trust", "in.json", "out.json"]).unwrap(),
            Command::Trust {
                request: PathBuf::from("in.json"),
                answer: PathBuf::from("out.json"),
            }
        );
        for bad in [
            &["trust"][..],
            &["trust", "in.json"],
            &["trust", "a", "b", "c"],
        ] {
            assert!(parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn grant_is_one_of_the_three_decisions() {
        for (word, decision) in [
            ("allow", Decision::Allow),
            ("substitute", Decision::Substitute),
            ("deny", Decision::Deny),
        ] {
            let Command::Run(launch) = parse(&["--app", "x", "--grant", word]).unwrap() else {
                panic!("a run");
            };
            assert_eq!(launch.grant, Some(decision));
        }
        assert!(parse(&["--app", "x", "--grant", "maybe"])
            .unwrap_err()
            .contains("not a decision"));
        assert!(parse(&["--app", "x", "--grant"])
            .unwrap_err()
            .contains("requires a value"));
        let Command::Run(launch) = parse(&["--app", "x", "--", "--grant", "deny"]).unwrap() else {
            panic!("a run");
        };
        assert_eq!(launch.grant, None, "after -- it is the application's own");
    }

    #[test]
    fn no_prompt_belongs_to_the_launcher_and_the_consent_window_takes_two_files() {
        let Command::Run(launch) = parse(&["--app", "x", "--no-prompt"]).unwrap() else {
            panic!("a run");
        };
        assert!(launch.no_prompt);
        let Command::Run(after) = parse(&["--app", "x", "--", "--no-prompt"]).unwrap() else {
            panic!("a run");
        };
        assert!(!after.no_prompt, "after -- it is the application's own");
        assert_eq!(
            parse(&["consent", "in.json", "out.json"]).unwrap(),
            Command::Consent {
                request: PathBuf::from("in.json"),
                answer: PathBuf::from("out.json"),
            }
        );
        for bad in [
            &["consent"][..],
            &["consent", "in.json"],
            &["consent", "a", "b", "c"],
        ] {
            assert!(parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn permissions_has_three_verbs_with_strict_arguments() {
        assert_eq!(
            parse(&["permissions", "list"]).unwrap(),
            Command::Permissions(PermissionsCommand::List { app: None })
        );
        assert_eq!(
            parse(&["permissions", "list", "site"]).unwrap(),
            Command::Permissions(PermissionsCommand::List {
                app: Some(PathBuf::from("site"))
            })
        );
        assert_eq!(
            parse(&["permissions", "set", "site", "app.env:HOME", "substitute"]).unwrap(),
            Command::Permissions(PermissionsCommand::Set {
                app: PathBuf::from("site"),
                right: Right::scoped("app.env", "HOME"),
                decision: Decision::Substitute,
            })
        );
        assert_eq!(
            parse(&["permissions", "reset", "site"]).unwrap(),
            Command::Permissions(PermissionsCommand::Reset {
                app: PathBuf::from("site")
            })
        );
        for bad in [
            &["permissions"][..],
            &["permissions", "grant"],
            &["permissions", "set", "site", "app.env:HOME"],
            &["permissions", "set", "site", "not a right", "allow"],
            &["permissions", "set", "site", "secrets", "maybe"],
            &["permissions", "set"],
            &["permissions", "reset"],
            &["permissions", "reset", "site", "extra"],
            &["permissions", "list", "site", "extra"],
        ] {
            assert!(parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn ai_limits_and_log_parse() {
        assert_eq!(
            parse(&[
                "ai",
                "limits",
                "somewhere",
                "--requests",
                "5",
                "--tokens",
                "0"
            ])
            .unwrap(),
            Command::Ai(AiCommand::Limits {
                app: PathBuf::from("somewhere"),
                requests: Some(5),
                tokens: Some(0),
            })
        );
        assert_eq!(
            parse(&["ai", "log", "somewhere"]).unwrap(),
            Command::Ai(AiCommand::Log {
                app: PathBuf::from("somewhere"),
            })
        );
        for bad in [
            &["ai", "limits", "site", "--requests", "5", "--requests", "6"][..],
            &["ai", "limits", "site", "--wat", "1"][..],
            &["ai", "history", "site"][..],
            &["ai", "log"][..],
            &["ai", "log", "site", "extra"][..],
            &["ai", "limits", "site", "--requests"][..],
        ] {
            assert!(parse(bad).is_err(), "{bad:?}");
        }
    }

    #[test]
    fn the_development_server_must_be_loopback_http_without_credentials() {
        for bad in [
            "https://127.0.0.1:3000/",
            "http://localhost:3000/",
            "http://192.168.0.1:3000/",
            "http://user:pass@127.0.0.1:3000/",
            "http://user@127.0.0.1:3000/",
            "ftp://127.0.0.1/",
            "not a url",
        ] {
            assert!(
                parse(&["--app", "x", "--dev-url", bad]).is_err(),
                "accepted {bad}"
            );
        }
    }

    #[test]
    fn a_target_shaped_like_an_installed_id_parses_as_a_path_and_the_usage_names_the_id_form() {
        let Command::Run(run) = parse(&["run", "org.example.app"]).unwrap() else {
            panic!("run")
        };
        assert_eq!(run.target, PathBuf::from("org.example.app"));
        assert!(USAGE.contains("<PATH|ID>"));
        assert!(USAGE.contains("`alef install`"));
    }

    #[test]
    fn only_the_positional_run_form_carries_the_installed_id_target() {
        let Command::Run(app) = parse(&["--app", "org.example.app"]).unwrap() else {
            panic!("run")
        };
        assert!(!app.positional);
        let Command::Run(run) = parse(&["run", "org.example.app"]).unwrap() else {
            panic!("run")
        };
        assert!(run.positional);
    }

    #[test]
    fn after_run_the_internal_command_names_are_targets_not_subcommands() {
        for name in ["trust", "consent"] {
            let Command::Run(run) = parse(&["run", name]).unwrap() else {
                panic!("run {name}")
            };
            assert_eq!(run.target, PathBuf::from(name));
            assert!(run.positional, "{name}");
        }
        assert!(matches!(
            parse(&["trust", "a", "b"]),
            Ok(Command::Trust {
                request: _,
                answer: _
            })
        ));
        assert!(matches!(
            parse(&["consent", "a", "b"]),
            Ok(Command::Consent {
                request: _,
                answer: _
            })
        ));
    }
}
