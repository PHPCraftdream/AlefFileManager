// SPDX-License-Identifier: MIT OR Apache-2.0
//! Command line of `alef`.
use std::{ffi::OsString, path::PathBuf};

use alef_core::security::consent::{Decision, Right};
use url::Url;

use crate::consent::decision_named;

pub const USAGE: &str =
    "alef --app <DIRECTORY> [--dev-url <http://127.0.0.1:PORT>] [--grant <DECISION>] [--no-prompt] [-- <APPLICATION ARGUMENTS>...]\n\
alef permissions list [<DIRECTORY>]\n\
alef permissions set <DIRECTORY> <RIGHT> <DECISION>\n\
alef permissions reset <DIRECTORY>\n\
Runs the application in DIRECTORY: its alef.ktav manifest and assets, in embedded Servo.\n\
  --app DIRECTORY   application directory containing alef.ktav\n\
  --dev-url URL     load the document from a development server on 127.0.0.1 instead of the files\n\
  --grant DECISION  decide every right the application asks for and you have not decided on yet:\n\
                    allow, substitute (a stand-in the application cannot tell from the real thing)\n\
                    or deny; the decision is remembered\n\
  --no-prompt       never show the permission window: an application with rights you have not\n\
                    decided on does not start\n\
  --                everything after it is the command line of the application\n\
                    (parsed by the `arguments` of its manifest; `-- --help` prints its usage)\n\
permissions: what you decided for an application. RIGHT is `permission` or `permission:scope`\n\
  (clipboard.read, app.env:HOME, fs.read:$DOCUMENTS/**); `list` without DIRECTORY lists the applications\n\
  you have decided for, `reset` makes the next start ask again";

/// A validated launch request.
#[derive(Debug, PartialEq, Eq)]
pub struct Launch {
    pub app_dir: PathBuf,
    pub dev_url: Option<Url>,
    /// The command line of the application: everything after `--`.
    pub app_args: Vec<OsString>,
    /// `--grant`: the decision for every right not decided yet.
    pub grant: Option<Decision>,
    /// `--no-prompt`: the permission window is never shown.
    pub no_prompt: bool,
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

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Run(Launch),
    Permissions(PermissionsCommand),
    /// `alef consent <REQUEST> <ANSWER>`, started by the launcher itself: the permission window.
    Consent {
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

fn folder(arguments: &mut impl Iterator<Item = OsString>, verb: &str) -> Result<PathBuf, String> {
    arguments
        .next()
        .map(PathBuf::from)
        .ok_or_else(|| format!("permissions {verb}: <DIRECTORY> is required"))
}

fn permissions(mut arguments: impl Iterator<Item = OsString>) -> Result<Command, String> {
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
            PermissionsCommand::Set {
                app,
                right: right.parse()?,
                decision: decision_named(&decision)?,
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

/// Parses the arguments after the program name.
pub fn parse_args(arguments: impl IntoIterator<Item = OsString>) -> Result<Command, String> {
    let mut arguments = arguments.into_iter().peekable();
    if arguments
        .peek()
        .is_some_and(|first| first.to_str() == Some("permissions"))
    {
        arguments.next();
        return permissions(arguments);
    }
    if arguments
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
    let mut app_dir = None;
    let mut dev = None;
    let mut grant = None;
    let mut no_prompt = false;
    let mut app_args = Vec::new();
    while let Some(argument) = arguments.next() {
        match argument.to_str() {
            Some("--") => {
                app_args.extend(arguments.by_ref());
                break;
            }
            Some("--app") => app_dir = Some(PathBuf::from(value(&mut arguments, "--app")?)),
            Some("--dev-url") => {
                let text = value(&mut arguments, "--dev-url")?;
                dev = Some(dev_url(&text.to_string_lossy())?);
            }
            Some("--grant") => {
                let word = text_of(value(&mut arguments, "--grant")?, "--grant")?;
                grant = Some(decision_named(&word)?);
            }
            Some("--no-prompt") => no_prompt = true,
            Some("--help" | "-h") => return Ok(Command::Help),
            _ => return Err(format!("unknown argument: {}", argument.to_string_lossy())),
        }
    }
    let app_dir = app_dir.ok_or_else(|| "--app <DIRECTORY> is required".to_owned())?;
    Ok(Command::Run(Launch {
        app_dir,
        dev_url: dev,
        app_args,
        grant,
        no_prompt,
    }))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(arguments: &[&str]) -> Result<Command, String> {
        parse_args(arguments.iter().map(OsString::from))
    }

    #[test]
    fn app_and_dev_url_are_read() {
        let command = parse(&["--app", "site", "--dev-url", "http://127.0.0.1:3000/"]).unwrap();
        assert_eq!(
            command,
            Command::Run(Launch {
                app_dir: PathBuf::from("site"),
                dev_url: Some(Url::parse("http://127.0.0.1:3000/").unwrap()),
                app_args: Vec::new(),
                grant: None,
                no_prompt: false,
            })
        );
        assert_eq!(
            parse(&["--app", "site"]).unwrap(),
            Command::Run(Launch {
                app_dir: PathBuf::from("site"),
                dev_url: None,
                app_args: Vec::new(),
                grant: None,
                no_prompt: false,
            })
        );
    }

    #[test]
    fn everything_after_the_double_dash_belongs_to_the_application() {
        let command = parse(&["--app", "site", "--", "--help", "--app", "x", "-h", "--"]).unwrap();
        let Command::Run(launch) = command else {
            panic!("a run, not the launcher help");
        };
        assert_eq!(launch.app_dir, PathBuf::from("site"));
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
}
