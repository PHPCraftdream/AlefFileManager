// SPDX-License-Identifier: MIT OR Apache-2.0
//! Command line of `alef`.
use std::{ffi::OsString, path::PathBuf};

use url::Url;

pub const USAGE: &str = "alef --app <DIRECTORY> [--dev-url <http://127.0.0.1:PORT>]\n\
Runs the application in DIRECTORY: its alef.ktav manifest and assets, in embedded Servo.\n\
  --app DIRECTORY   application directory containing alef.ktav\n\
  --dev-url URL     load the document from a development server on 127.0.0.1 instead of the files";

/// A validated launch request.
#[derive(Debug, PartialEq, Eq)]
pub struct Launch {
    pub app_dir: PathBuf,
    pub dev_url: Option<Url>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum Command {
    Run(Launch),
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

/// Parses the arguments after the program name.
pub fn parse_args(arguments: impl IntoIterator<Item = OsString>) -> Result<Command, String> {
    let mut arguments = arguments.into_iter();
    let mut app_dir = None;
    let mut dev = None;
    while let Some(argument) = arguments.next() {
        match argument.to_str() {
            Some("--app") => app_dir = Some(PathBuf::from(value(&mut arguments, "--app")?)),
            Some("--dev-url") => {
                let text = value(&mut arguments, "--dev-url")?;
                dev = Some(dev_url(&text.to_string_lossy())?);
            }
            Some("--help" | "-h") => return Ok(Command::Help),
            _ => return Err(format!("unknown argument: {}", argument.to_string_lossy())),
        }
    }
    let app_dir = app_dir.ok_or_else(|| "--app <DIRECTORY> is required".to_owned())?;
    Ok(Command::Run(Launch {
        app_dir,
        dev_url: dev,
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
            })
        );
        assert_eq!(
            parse(&["--app", "site"]).unwrap(),
            Command::Run(Launch {
                app_dir: PathBuf::from("site"),
                dev_url: None
            })
        );
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
