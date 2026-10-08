// SPDX-License-Identifier: MIT OR Apache-2.0
//! Executable scopes: `*`, a bare program name, an absolute path (compared component-wise) or
//! `sidecar:<name>`.
use super::{
    clean, invalid, path,
    sidecar::{is_sidecar_reference, sidecar_name},
};
use crate::AlefError;

/// Path separators of the host OS; a backslash is an ordinary name character on Unix.
const SEPARATORS: &[char] = if cfg!(windows) { &['/', '\\'] } else { &['/'] };

/// One entry of `cli.exec`.
#[derive(Debug, Clone)]
pub(crate) enum ExecScope {
    /// Explicit `*`: any program.
    Any,
    /// Bare name resolved by the OS (`git`); never matches a path.
    Name(String),
    /// Absolute path, compared by normalized components.
    Path(Vec<String>),
    /// `sidecar:<name>`: the program `bin/<name>` of the application; never matches a plain name.
    Sidecar(String),
}

fn drive_form(text: &str) -> bool {
    let bytes = text.as_bytes();
    bytes.len() >= 3
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && matches!(bytes[2], b'/' | b'\\')
}

fn absolute(text: &str) -> bool {
    text.starts_with('/') || (cfg!(windows) && (text.starts_with('\\') || drive_form(text)))
}

/// Root marker plus names of an absolute path; `.`/`..` and unsafe Windows names are refused.
/// A leading UNC `\\` stays distinct from a drive-relative `\`.
fn components(text: &str) -> Option<Vec<String>> {
    let mut out = Vec::new();
    let rest = if cfg!(windows) && drive_form(text) {
        out.push(text[..2].to_owned());
        &text[2..]
    } else {
        let lead = text.chars().take_while(|c| SEPARATORS.contains(c)).count();
        out.push(
            if cfg!(windows) && lead >= 2 {
                "//"
            } else {
                "/"
            }
            .to_owned(),
        );
        &text[lead..]
    };
    if rest.ends_with(SEPARATORS) {
        return None; // a trailing separator names a directory, never a program
    }
    for part in rest.split(SEPARATORS).filter(|part| !part.is_empty()) {
        if part == "." || part == ".." || path::windows_unsafe(part) {
            return None;
        }
        out.push(part.to_owned());
    }
    Some(out)
}

impl ExecScope {
    pub(crate) fn parse(pattern: &str) -> Result<Self, AlefError> {
        if pattern == "*" {
            return Ok(Self::Any);
        }
        if !clean(pattern) {
            return Err(invalid("invalid executable scope"));
        }
        if is_sidecar_reference(pattern) {
            return sidecar_name(pattern)
                .map(|name| Self::Sidecar(name.to_owned()))
                .ok_or_else(|| invalid("invalid sidecar name"));
        }
        if absolute(pattern) {
            return components(pattern)
                .map(Self::Path)
                .ok_or_else(|| invalid("executable path must be normalized"));
        }
        if pattern.contains(SEPARATORS) || pattern == "." || pattern == ".." {
            return Err(invalid(
                "executable scope is neither a bare name nor an absolute path",
            ));
        }
        Ok(Self::Name(pattern.to_owned()))
    }

    pub(crate) fn matches(&self, target: &str) -> bool {
        if !clean(target) {
            return false;
        }
        match self {
            Self::Any => true,
            Self::Name(name) => {
                !absolute(target) && !target.contains(SEPARATORS) && path::same(name, target)
            }
            Self::Sidecar(name) => {
                sidecar_name(target).is_some_and(|actual| path::same(name, actual))
            }
            Self::Path(expected) => {
                absolute(target)
                    && components(target).is_some_and(|actual| {
                        actual.len() == expected.len()
                            && actual.iter().zip(expected).all(|(a, b)| path::same(a, b))
                    })
            }
        }
    }
}
