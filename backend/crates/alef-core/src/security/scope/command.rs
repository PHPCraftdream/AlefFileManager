// SPDX-License-Identifier: MIT OR Apache-2.0
//! Declared commands (`permissions.cli.commands`): fixed programs with an argument template. The
//! page picks one by name and fills the `{param}` elements; it never chooses the program or adds
//! arguments.
use std::{collections::BTreeMap, fmt};

use serde_json::json;

use super::{
    clean,
    exec::ExecScope,
    sidecar::{is_sidecar_reference, sidecar_name},
};
use crate::{
    security::manifest::{CliCommand, CliPermissions},
    AlefError, ErrorCode,
};

/// Program of a declared command.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Program {
    /// A bare name found on the `PATH` of the runtime, or an absolute path (as in `cli.exec`).
    Plain(String),
    /// `sidecar:<name>`: the program `bin/<name>` of the application.
    Sidecar(String),
}

impl fmt::Display for Program {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Plain(program) => f.write_str(program),
            Self::Sidecar(name) => write!(f, "{}{name}", super::sidecar::SIDECAR_PREFIX),
        }
    }
}

/// One element of the argument template.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ArgTemplate {
    /// Passed as it is.
    Literal(String),
    /// An element that is exactly `{param}`: replaced by the value the page passes for `param`.
    Param(String),
}

/// A command the manifest declares.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeclaredCommand {
    pub name: String,
    pub program: Program,
    pub args: Vec<ArgTemplate>,
    /// Text the user sees when deciding.
    pub description: String,
}

/// Why a parameter list does not fit a template; all of these are `INVALID_ARGUMENT`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ExpandError {
    /// The template has `{param}` and the page passed no value.
    MissingParam(String),
    /// The page passed a value for a name the template does not use.
    UnknownParam(String),
    /// The value contains a NUL character.
    NulInParam(String),
    /// The value starts with a hyphen where the template has not ended the options with `--`: the
    /// program would take it for an option of its own.
    OptionLikeParam(String),
}

impl fmt::Display for ExpandError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::MissingParam(name) => write!(f, "missing parameter {name}"),
            Self::UnknownParam(name) => write!(f, "unknown parameter {name}"),
            Self::NulInParam(name) => write!(f, "parameter {name} contains a NUL character"),
            Self::OptionLikeParam(name) => write!(
                f,
                "parameter {name} starts with a hyphen, which the program would read as an option"
            ),
        }
    }
}

impl std::error::Error for ExpandError {}

impl From<ExpandError> for AlefError {
    fn from(error: ExpandError) -> Self {
        let (ExpandError::MissingParam(name)
        | ExpandError::UnknownParam(name)
        | ExpandError::NulInParam(name)
        | ExpandError::OptionLikeParam(name)) = &error;
        let details = json!({ "param": name });
        Self::new(ErrorCode::InvalidArgument, error.to_string()).with_details(details)
    }
}

/// `[a-z0-9][a-z0-9-]*`.
fn valid_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars
        .next()
        .is_some_and(|c| c.is_ascii_lowercase() || c.is_ascii_digit())
        && chars.all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
}

/// `[a-z][a-zA-Z0-9]*`.
fn valid_param(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase()) && chars.all(|c| c.is_ascii_alphanumeric())
}

/// Only an element exactly matching `{param}`, with `param` matching `[a-z][a-zA-Z0-9]*`,
/// is substituted; all other elements are literals.
fn parse_arg(text: &str) -> Result<ArgTemplate, &'static str> {
    if text.contains('\0') {
        return Err("must not contain a NUL character");
    }
    if let Some(inside) = text
        .strip_prefix('{')
        .and_then(|text| text.strip_suffix('}'))
    {
        if valid_param(inside) {
            return Ok(ArgTemplate::Param(inside.to_owned()));
        }
    }
    Ok(ArgTemplate::Literal(text.to_owned()))
}

fn parse_program(text: &str) -> Result<Program, &'static str> {
    if is_sidecar_reference(text) {
        return sidecar_name(text)
            .map(|name| Program::Sidecar(name.to_owned()))
            .ok_or("invalid sidecar name");
    }
    match ExecScope::parse(text) {
        Ok(ExecScope::Name(_) | ExecScope::Path(_)) => Ok(Program::Plain(text.to_owned())),
        Ok(_) => Err("must name one program, not *"),
        Err(_) => Err("must be a bare program name, an absolute path or sidecar:<name>"),
    }
}

fn quoted(arg: &str) -> String {
    if arg.is_empty() || arg.contains(|c: char| c.is_whitespace() || c == '"') {
        format!("\"{}\"", arg.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        arg.to_owned()
    }
}

impl DeclaredCommand {
    /// Program and template as one line, `git status --short {path}`.
    pub fn command_line(&self) -> String {
        let mut line = quoted(&self.program.to_string());
        for arg in &self.args {
            line.push(' ');
            match arg {
                ArgTemplate::Literal(text) => line.push_str(&quoted(text)),
                ArgTemplate::Param(name) => line.push_str(&format!("{{{name}}}")),
            }
        }
        line
    }

    /// What the user sees where the right is listed:
    /// `status — Shows the state of the folder (git status --short {path})`.
    pub fn summary(&self) -> String {
        format!(
            "{} — {} ({})",
            self.name,
            self.description,
            self.command_line()
        )
    }

    /// The names of the parameters of the template, in order of first use.
    pub fn params(&self) -> Vec<&str> {
        let mut names: Vec<&str> = Vec::new();
        for arg in &self.args {
            if let ArgTemplate::Param(name) = arg {
                if !names.contains(&name.as_str()) {
                    names.push(name);
                }
            }
        }
        names
    }

    /// The argument list for `params`: a parameter element is replaced by its value, a literal is
    /// kept. No parameter may be missing and none may be passed that the template does not use;
    /// values with a NUL are refused, and so are values starting with a hyphen unless a literal
    /// `--` comes before the parameter in the template.
    pub fn expand(&self, params: &BTreeMap<String, String>) -> Result<Vec<String>, ExpandError> {
        let used = self.params();
        if let Some(name) = params.keys().find(|name| !used.contains(&name.as_str())) {
            return Err(ExpandError::UnknownParam(name.clone()));
        }
        let mut options_ended = false;
        self.args
            .iter()
            .map(|arg| match arg {
                ArgTemplate::Literal(text) => {
                    options_ended |= text == "--";
                    Ok(text.clone())
                }
                ArgTemplate::Param(name) => {
                    let value = params
                        .get(name)
                        .ok_or_else(|| ExpandError::MissingParam(name.clone()))?;
                    if value.contains('\0') {
                        return Err(ExpandError::NulInParam(name.clone()));
                    }
                    if !options_ended && value.starts_with('-') {
                        return Err(ExpandError::OptionLikeParam(name.clone()));
                    }
                    Ok(value.clone())
                }
            })
            .collect()
    }

    fn parse(raw: &CliCommand) -> Result<Self, (String, &'static str)> {
        let at = |field: &str| {
            let field = field.to_owned();
            move |reason| (field, reason)
        };
        if !valid_name(&raw.name) {
            return Err(at("name")(
                "must be lowercase letters, digits and hyphens, not starting with a hyphen",
            ));
        }
        let program = parse_program(&raw.program).map_err(at("program"))?;
        let args = raw
            .args
            .iter()
            .enumerate()
            .map(|(index, arg)| parse_arg(arg).map_err(at(&format!("args[{index}]"))))
            .collect::<Result<_, _>>()?;
        if !clean(&raw.description) || raw.description.trim().is_empty() {
            return Err(at("description")(
                "must be non-empty text without control characters",
            ));
        }
        Ok(Self {
            name: raw.name.clone(),
            program,
            args,
            description: raw.description.clone(),
        })
    }
}

fn invalid_at(path: &str, reason: &str) -> AlefError {
    AlefError::new(ErrorCode::ManifestInvalid, format!("{path}: {reason}"))
        .with_details(json!({ "path": path }))
}

/// Validates the `cli` part of the manifest beyond its shape: the sidecar entries of `exec` and
/// the declared commands. The error carries the path (`permissions.cli.commands[0].name`).
pub(crate) fn parse_cli(cli: &CliPermissions) -> Result<Vec<DeclaredCommand>, AlefError> {
    for (index, entry) in cli.exec.iter().enumerate() {
        if is_sidecar_reference(entry) && sidecar_name(entry).is_none() {
            return Err(invalid_at(
                &format!("permissions.cli.exec[{index}]"),
                "invalid sidecar name",
            ));
        }
    }
    let mut commands: Vec<DeclaredCommand> = Vec::new();
    for (index, raw) in cli.commands.iter().enumerate() {
        let path = |field: &str| format!("permissions.cli.commands[{index}].{field}");
        let command = DeclaredCommand::parse(raw)
            .map_err(|(field, reason)| invalid_at(&path(&field), reason))?;
        if commands.iter().any(|other| other.name == command.name) {
            return Err(invalid_at(&path("name"), "duplicate command"));
        }
        commands.push(command);
    }
    Ok(commands)
}
