// SPDX-License-Identifier: MIT OR Apache-2.0
//! Filesystem scopes: canonicalization (`..` applied, symlinks resolved) and glob patterns.
use super::{clean, invalid};
use crate::AlefError;
use std::{
    io::ErrorKind,
    path::{Component, Path, PathBuf, Prefix},
};

/// Whether path comparison ignores ASCII case (Windows and macOS file systems).
pub(crate) const CASE_INSENSITIVE: bool = cfg!(any(windows, target_os = "macos"));

/// Compares two path components under the platform's case rules.
pub(crate) fn same(a: &str, b: &str) -> bool {
    if CASE_INSENSITIVE {
        a.eq_ignore_ascii_case(b)
    } else {
        a == b
    }
}

/// Names Windows resolves to something other than the plain file they spell (aliases, streams, devices).
pub(crate) fn windows_unsafe(name: &str) -> bool {
    if !cfg!(windows) {
        return false;
    }
    if name.ends_with(['.', ' ']) || name.contains([':', '<', '>', '"', '|', '?', '*']) {
        return true;
    }
    let stem = name.split('.').next().unwrap_or("").to_ascii_uppercase();
    let numbered = |prefix: &str| {
        stem.strip_prefix(prefix)
            .is_some_and(|n| n.len() == 1 && n.as_bytes()[0].is_ascii_digit())
    };
    matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL") || numbered("COM") || numbered("LPT")
}

/// Resolves an absolute path to its physical form: `.`/`..` are applied and every existing
/// component is canonicalized one at a time, so a symlink reached after a `..` is followed too.
/// A missing suffix is kept lexically. Anything ambiguous (relative, NUL, unreadable component,
/// dangling symlink, Windows alias/device names) yields `None`.
pub(crate) fn canonical(path: &Path) -> Option<PathBuf> {
    if !path.is_absolute() || !clean(&path.as_os_str().to_string_lossy()) {
        return None;
    }
    let mut resolved = PathBuf::new();
    for component in path.components() {
        match component {
            Component::Prefix(prefix) => {
                if matches!(prefix.kind(), Prefix::DeviceNS(_)) {
                    return None;
                }
                resolved.push(component.as_os_str());
            }
            Component::RootDir => resolved.push(component.as_os_str()),
            Component::CurDir => {}
            Component::ParentDir => {
                resolved.pop();
            }
            Component::Normal(name) => {
                if windows_unsafe(&name.to_string_lossy()) {
                    return None;
                }
                let next = resolved.join(name);
                match std::fs::symlink_metadata(&next) {
                    Ok(_) => resolved = std::fs::canonicalize(&next).ok()?,
                    Err(error) if error.kind() == ErrorKind::NotFound => resolved = next,
                    Err(_) => return None,
                }
            }
        }
    }
    Some(strip_verbatim(resolved))
}

fn strip_verbatim(path: PathBuf) -> PathBuf {
    #[cfg(windows)]
    {
        let text = path.to_string_lossy();
        if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
            return PathBuf::from(format!(r"\\{rest}"));
        }
        if let Some(rest) = text.strip_prefix(r"\\?\") {
            return PathBuf::from(rest);
        }
    }
    path
}

/// Resolves the entry a path names without following it when it is a link: the directory it lies
/// in is resolved like [`canonical`] does, the name is kept. For what acts on a link itself (remove,
/// rename, `lstat`). A path that ends in `..` or has no parent names no entry.
pub(crate) fn canonical_entry(path: &Path) -> Option<PathBuf> {
    let name = path.file_name()?;
    if windows_unsafe(&name.to_string_lossy()) {
        return None;
    }
    Some(canonical(path.parent()?)?.join(name))
}

/// Path split into comparable string components (prefix and root included).
pub(crate) fn parts(path: &Path) -> Vec<String> {
    path.components()
        .filter_map(|component| match component {
            Component::Prefix(_) | Component::RootDir | Component::Normal(_) => {
                Some(component.as_os_str().to_string_lossy().into_owned())
            }
            _ => None,
        })
        .collect()
}

/// A filesystem scope: a canonical literal base plus optional glob components (`*`, `**`).
#[derive(Debug, Clone)]
pub(crate) struct PathPattern {
    base: Vec<String>,
    rest: Vec<String>,
}

impl PathPattern {
    /// Parses `pattern`; with `root` (an expanded `$VAR`) the pattern is relative to it and the
    /// root is taken literally, so glob characters in a variable's value never widen the scope.
    pub(crate) fn parse(root: Option<&Path>, pattern: &str) -> Result<Self, AlefError> {
        if !pattern.is_empty() && !clean(pattern) {
            return Err(invalid("path scope contains control characters"));
        }
        let mut base = root.map(Path::to_path_buf).unwrap_or_default();
        let mut rest: Vec<String> = Vec::new();
        for component in Path::new(pattern).components() {
            match component {
                Component::Prefix(_) | Component::RootDir if root.is_none() && rest.is_empty() => {
                    base.push(component.as_os_str());
                }
                Component::Prefix(_) | Component::RootDir => {
                    return Err(invalid("path scope must not embed a root after a variable"));
                }
                Component::CurDir => {}
                Component::ParentDir if rest.is_empty() => base.push(".."),
                Component::ParentDir => return Err(invalid("`..` after a wildcard in a scope")),
                Component::Normal(name) => {
                    let name = name.to_string_lossy();
                    if name.contains("**") && name != "**" {
                        return Err(invalid("`**` must be a whole path component"));
                    }
                    if rest.is_empty() && !name.contains('*') {
                        base.push(name.as_ref());
                    } else if !(name == "**" && rest.last().is_some_and(|last| last == "**")) {
                        rest.push(name.into_owned());
                    }
                }
            }
        }
        let canonical = canonical(&base)
            .ok_or_else(|| invalid("path scope must be an absolute, resolvable path"))?;
        Ok(Self {
            base: parts(&canonical),
            rest,
        })
    }

    /// The components of a path this scope matches that come after its literal base: where the
    /// path lies inside the scope.
    pub(crate) fn inside(&self, candidate: &[String]) -> std::path::PathBuf {
        candidate[self.base.len().min(candidate.len())..]
            .iter()
            .collect()
    }

    /// Tests already canonical components.
    pub(crate) fn matches_canonical(&self, candidate: &[String]) -> bool {
        candidate.len() >= self.base.len()
            && self.base.iter().zip(candidate).all(|(a, b)| same(a, b))
            && match_rest(&self.rest, &candidate[self.base.len()..])
    }
}

fn match_rest(pattern: &[String], candidate: &[String]) -> bool {
    match pattern.split_first() {
        None => candidate.is_empty(),
        Some((head, tail)) if head == "**" => {
            match_rest(tail, candidate)
                || candidate
                    .split_first()
                    .is_some_and(|(_, more)| match_rest(pattern, more))
        }
        Some((head, tail)) => candidate
            .split_first()
            .is_some_and(|(name, more)| glob(head, name) && match_rest(tail, more)),
    }
}

/// Matches one component against a pattern whose only wildcard is `*` (any run, possibly empty).
fn glob(pattern: &str, name: &str) -> bool {
    let eq = |a: char, b: char| {
        if CASE_INSENSITIVE {
            a.eq_ignore_ascii_case(&b)
        } else {
            a == b
        }
    };
    let (p, n): (Vec<char>, Vec<char>) = (pattern.chars().collect(), name.chars().collect());
    let (mut pi, mut ni, mut star, mut mark) = (0, 0, None, 0);
    while ni < n.len() {
        if pi < p.len() && p[pi] == '*' {
            star = Some(pi);
            mark = ni;
            pi += 1;
        } else if pi < p.len() && eq(p[pi], n[ni]) {
            pi += 1;
            ni += 1;
        } else if let Some(s) = star {
            pi = s + 1;
            mark += 1;
            ni = mark;
        } else {
            return false;
        }
    }
    p[pi..].iter().all(|c| *c == '*')
}
