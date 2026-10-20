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
  --devtools[=PORT] enable local Servo debugging (default port 7000; development directories only)\n\
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
    /// Opt-in DevTools loopback port (7000 by default when enabled).
    pub devtools_port: Option<u16>,
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
            if self.devtools_port.is_some() {
                return Err(
                    "--devtools is not accepted for a package: the manifest and its assets are the only source"
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

fn devtools_port(text: &str) -> Result<u16, String> {
    if text.is_empty() || !text.bytes().all(|byte| byte.is_ascii_digit()) {
        return Err("--devtools PORT must contain ASCII digits".to_owned());
    }
    let port = text
        .parse::<u16>()
        .map_err(|_| "--devtools PORT must be in 1..65535".to_owned())?;
    if port == 0 {
        return Err("--devtools PORT must be in 1..65535".to_owned());
    }
    Ok(port)
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
    let mut devtools = None;
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
                flag @ ("--app" | "--dev-url" | "--devtools" | "--grant" | "--no-prompt"
                | "--launch-mode" | "--trust-key" | "--allow-unsigned"),
            ) if !app_args.is_empty() => {
                return Err(format!("{flag} is not accepted after a URL"));
            }
            Some(text) if text.starts_with("--devtools") && !app_args.is_empty() => {
                return Err("--devtools is not accepted after a URL".to_owned());
            }
            Some("--app") if target.is_some() => return Err("--app is given twice".to_owned()),
            Some("--dev-url") if dev.is_some() => return Err("--dev-url is given twice".to_owned()),
            Some("--grant") if grant.is_some() => return Err("--grant is given twice".to_owned()),
            Some(text) if text.starts_with("--devtools") && devtools.is_some() => {
                return Err("--devtools is given twice".to_owned());
            }
            Some("--app") => target = Some(PathBuf::from(value(&mut arguments, "--app")?)),
            Some("--dev-url") => {
                let text = value(&mut arguments, "--dev-url")?;
                dev = Some(dev_url(&text.to_string_lossy())?);
            }
            Some("--devtools") => devtools = Some(7000),
            Some(text) if text.starts_with("--devtools=") => {
                devtools = Some(devtools_port(&text[11..])?);
            }
            Some(text) if text.starts_with("--devtools") => {
                return Err("--devtools accepts only --devtools=PORT".to_owned());
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
        devtools_port: devtools,
        app_args,
        grant,
        trust_key,
        allow_unsigned,
        no_prompt,
        launch_window,
        positional,
    }))
}
