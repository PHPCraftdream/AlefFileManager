// SPDX-License-Identifier: MIT OR Apache-2.0
//! Command-line parsing by the `arguments` schema of the manifest, and the generated usage text.
//!
//! Grammar: `--name value`, `--name=value`, `-s value`, `-s=value`; a flag takes no separate value
//! (`--flag`, `--flag=true`, `--flag=false`); `--` ends the options; a repeated option keeps its last
//! value. Short options are not clustered. `--help`/`-h` and `--version`/`-V` are generated.
use std::{collections::BTreeMap, ffi::OsString};

use alef_core::{
    security::manifest::{ArgKind, ArgOption, Arguments},
    AlefError, ErrorCode,
};
use serde::{Deserialize, Serialize};

/// A parsed option value, typed by the schema.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(untagged)]
#[ts(export, export_to = "modules.ts")]
pub enum ArgValue {
    Flag(bool),
    Number(f64),
    Text(String),
}

/// The command line of the application.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = "modules.ts")]
pub struct ParsedArgs {
    /// Every argument as given (lossy for text that is not Unicode).
    pub raw: Vec<String>,
    /// Options that were given, by long name.
    pub parsed: BTreeMap<String, ArgValue>,
    /// Arguments that are not options, in order.
    pub positional: Vec<String>,
}

/// What the command line asks for.
#[derive(Debug, PartialEq)]
pub enum Parsed {
    Run(ParsedArgs),
    /// `--help`: the text to print, exit code 0.
    Help(String),
    /// `--version`: the text to print, exit code 0.
    Version(String),
}

fn usage_error(message: String) -> AlefError {
    AlefError::new(ErrorCode::InvalidArgument, message)
}

fn find<'a>(schema: Option<&'a Arguments>, arg: &str) -> Option<&'a ArgOption> {
    let options = &schema?.options;
    match arg.strip_prefix("--") {
        Some(long) => options.iter().find(|option| option.name == long),
        None => options
            .iter()
            .find(|option| option.short.as_deref() == arg.strip_prefix('-')),
    }
}

fn typed(option: &ArgOption, shown: &str, value: &str) -> Result<ArgValue, AlefError> {
    Ok(match option.kind {
        ArgKind::String => ArgValue::Text(value.to_owned()),
        ArgKind::Number => match value.parse::<f64>() {
            Ok(number) if number.is_finite() => ArgValue::Number(number),
            _ => {
                return Err(usage_error(format!(
                    "{shown} expects a number, got {value:?}"
                )))
            }
        },
        ArgKind::Boolean => match value {
            "true" => ArgValue::Flag(true),
            "false" => ArgValue::Flag(false),
            _ => {
                return Err(usage_error(format!(
                    "{shown} expects true or false, got {value:?}"
                )))
            }
        },
    })
}

/// Parses `raw` (the arguments after the program name) by `schema`; errors are usage errors.
pub fn parse(
    schema: Option<&Arguments>,
    name: &str,
    version: &str,
    raw: &[OsString],
) -> Result<Parsed, AlefError> {
    let raw: Vec<String> = raw
        .iter()
        .map(|argument| argument.to_string_lossy().into_owned())
        .collect();
    let mut parsed = BTreeMap::new();
    let mut positional = Vec::new();
    let mut options_ended = false;
    let mut arguments = raw.iter();
    while let Some(argument) = arguments.next() {
        if options_ended || argument == "-" || !argument.starts_with('-') {
            positional.push(argument.clone());
            continue;
        }
        let (flag, inline) = match argument.split_once('=') {
            Some((flag, value)) => (flag, Some(value)),
            None => (argument.as_str(), None),
        };
        match flag {
            "--" if inline.is_none() => options_ended = true,
            "--help" | "-h" if inline.is_none() => {
                return Ok(Parsed::Help(help(schema, name, version)))
            }
            "--version" | "-V" if inline.is_none() => {
                return Ok(Parsed::Version(format!("{name} {version}")))
            }
            _ => {
                let option = find(schema, flag)
                    .ok_or_else(|| usage_error(format!("unknown option {flag} (see --help)")))?;
                let value = match (option.kind, inline) {
                    (ArgKind::Boolean, None) => ArgValue::Flag(true),
                    (_, Some(text)) => typed(option, flag, text)?,
                    (_, None) => {
                        let text = arguments.next().ok_or_else(|| {
                            usage_error(format!("option {flag} requires a value"))
                        })?;
                        typed(option, flag, text)?
                    }
                };
                parsed.insert(option.name.clone(), value);
            }
        }
    }
    if !positional.is_empty() && schema.and_then(|s| s.positional.as_ref()).is_none() {
        return Err(usage_error(format!(
            "unexpected argument {:?} (see --help)",
            positional[0]
        )));
    }
    Ok(Parsed::Run(ParsedArgs {
        raw,
        parsed,
        positional,
    }))
}

fn kind_name(kind: ArgKind) -> &'static str {
    match kind {
        ArgKind::String => "text",
        ArgKind::Number => "number",
        ArgKind::Boolean => "",
    }
}

/// The generated `--help` text.
pub fn help(schema: Option<&Arguments>, name: &str, version: &str) -> String {
    let mut usage = format!("{name} {version}\n\nUsage: {name} [OPTIONS]");
    if let Some(positional) = schema.and_then(|s| s.positional.as_ref()) {
        usage.push_str(&format!(" [{}...]", positional.name));
    }
    let mut rows: Vec<(String, String)> = schema
        .map(|s| s.options.as_slice())
        .unwrap_or_default()
        .iter()
        .map(|option| {
            let short = option
                .short
                .as_ref()
                .map_or_else(|| "    ".to_owned(), |short| format!("-{short}, "));
            let value = match kind_name(option.kind) {
                "" => String::new(),
                kind => format!(" <{kind}>"),
            };
            (
                format!("{short}--{}{value}", option.name),
                option.description.clone().unwrap_or_default(),
            )
        })
        .collect();
    rows.push(("-h, --help".to_owned(), "Print help".to_owned()));
    rows.push(("-V, --version".to_owned(), "Print version".to_owned()));
    let width = rows.iter().map(|(left, _)| left.len()).max().unwrap_or(0);
    usage.push_str("\n\nOptions:\n");
    for (left, right) in &rows {
        usage.push_str(&format!("  {left:<width$}  {right}\n"));
    }
    if let Some(positional) = schema.and_then(|s| s.positional.as_ref()) {
        if let Some(description) = &positional.description {
            usage.push_str(&format!(
                "\nArguments:\n  {}  {description}\n",
                positional.name
            ));
        }
    }
    usage.trim_end().to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use alef_core::security::manifest::ArgPositional;

    fn schema() -> Arguments {
        let option = |name: &str, short: Option<&str>, kind, description: Option<&str>| ArgOption {
            name: name.to_owned(),
            short: short.map(str::to_owned),
            kind,
            description: description.map(str::to_owned),
        };
        Arguments {
            options: vec![
                option(
                    "port",
                    Some("p"),
                    ArgKind::Number,
                    Some("Port to listen on"),
                ),
                option("name", None, ArgKind::String, None),
                option("verbose", Some("v"), ArgKind::Boolean, Some("Talk more")),
            ],
            positional: Some(ArgPositional {
                name: "files".to_owned(),
                description: Some("Files to open".to_owned()),
            }),
        }
    }

    fn run(arguments: &[&str]) -> Result<Parsed, AlefError> {
        let raw: Vec<OsString> = arguments.iter().map(OsString::from).collect();
        parse(Some(&schema()), "Example", "1.2.3", &raw)
    }

    fn parsed(arguments: &[&str]) -> ParsedArgs {
        match run(arguments).expect("parses") {
            Parsed::Run(args) => args,
            other => panic!("expected a run, got {other:?}"),
        }
    }

    fn refused(arguments: &[&str]) -> String {
        let error = run(arguments).expect_err("must be refused");
        assert_eq!(error.code, ErrorCode::InvalidArgument);
        error.message
    }

    #[test]
    fn options_take_values_in_every_spelling_and_are_typed_by_the_schema() {
        let args = parsed(&["--port", "8080", "--name=alef", "-v", "a.txt", "b.txt"]);
        assert_eq!(args.parsed["port"], ArgValue::Number(8080.0));
        assert_eq!(args.parsed["name"], ArgValue::Text("alef".to_owned()));
        assert_eq!(args.parsed["verbose"], ArgValue::Flag(true));
        assert_eq!(args.positional, ["a.txt", "b.txt"]);
        assert_eq!(args.raw.len(), 6);

        assert_eq!(
            parsed(&["-p", "-1.5"]).parsed["port"],
            ArgValue::Number(-1.5)
        );
        assert_eq!(parsed(&["-p=7"]).parsed["port"], ArgValue::Number(7.0));
        assert_eq!(
            parsed(&["--verbose=false"]).parsed["verbose"],
            ArgValue::Flag(false)
        );
        assert_eq!(
            parsed(&["--name", "-x"]).parsed["name"],
            ArgValue::Text("-x".to_owned())
        );
        assert_eq!(
            parsed(&["--name="]).parsed["name"],
            ArgValue::Text(String::new())
        );
        assert_eq!(
            parsed(&["--port=1", "--port=2"]).parsed["port"],
            ArgValue::Number(2.0)
        );
        assert!(parsed(&[]).parsed.is_empty());
    }

    #[test]
    fn a_flag_does_not_swallow_the_next_argument() {
        let args = parsed(&["--verbose", "file"]);
        assert_eq!(args.parsed["verbose"], ArgValue::Flag(true));
        assert_eq!(args.positional, ["file"]);
    }

    #[test]
    fn double_dash_and_a_lone_dash_make_positionals() {
        let args = parsed(&["--verbose", "--", "--port", "-v", "x"]);
        assert_eq!(args.positional, ["--port", "-v", "x"]);
        assert_eq!(args.parsed.len(), 1);
        assert_eq!(parsed(&["-"]).positional, ["-"]);
    }

    #[test]
    fn bad_command_lines_are_usage_errors_that_name_the_cause() {
        assert!(refused(&["--nope"]).contains("unknown option --nope"));
        assert!(refused(&["-x"]).contains("unknown option -x"));
        assert!(refused(&["-vp"]).contains("unknown option -vp"));
        assert!(refused(&["--port"]).contains("--port requires a value"));
        assert!(refused(&["--port", "abc"]).contains("expects a number"));
        assert!(refused(&["--port=NaN"]).contains("expects a number"));
        assert!(refused(&["--port=inf"]).contains("expects a number"));
        assert!(refused(&["--verbose=maybe"]).contains("true or false"));
        assert!(refused(&["--help=1"]).contains("unknown option --help"));
    }

    #[test]
    fn without_a_schema_every_option_and_positional_is_refused() {
        let raw = |text: &str| vec![OsString::from(text)];
        assert!(parse(None, "A", "1", &raw("--x")).is_err());
        assert!(parse(None, "A", "1", &raw("file")).is_err());
        assert!(matches!(parse(None, "A", "1", &[]), Ok(Parsed::Run(_))));
        let no_positional = Arguments {
            options: Vec::new(),
            positional: None,
        };
        assert!(parse(Some(&no_positional), "A", "1", &raw("file")).is_err());
    }

    #[test]
    fn help_and_version_are_generated_and_win_over_later_errors() {
        match run(&["--help", "--bogus"]).unwrap() {
            Parsed::Help(text) => {
                assert!(text.starts_with("Example 1.2.3\n\nUsage: Example [OPTIONS] [files...]"));
                assert!(text.contains("-p, --port <number>"));
                assert!(text.contains("Port to listen on"));
                assert!(text.contains("    --name <text>"));
                assert!(text.contains("-v, --verbose  "));
                assert!(text.contains("-h, --help"));
                assert!(text.contains("-V, --version"));
                assert!(text.contains("Files to open"));
            }
            other => panic!("expected help, got {other:?}"),
        }
        assert_eq!(run(&["-h"]).unwrap(), run(&["--help"]).unwrap());
        assert_eq!(
            run(&["-V"]).unwrap(),
            Parsed::Version("Example 1.2.3".to_owned())
        );
        assert_eq!(run(&["--version"]).unwrap(), run(&["-V"]).unwrap());
        // after `--` they are plain positionals
        assert_eq!(parsed(&["--", "--help"]).positional, ["--help"]);
    }

    #[test]
    fn help_without_a_schema_lists_only_the_generated_options() {
        let text = help(None, "A", "1");
        assert_eq!(
            text,
            "A 1\n\nUsage: A [OPTIONS]\n\nOptions:\n  -h, --help     Print help\n  -V, --version  Print version"
        );
    }
}
