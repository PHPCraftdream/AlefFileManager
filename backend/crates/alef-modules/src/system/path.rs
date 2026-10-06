// SPDX-License-Identifier: MIT OR Apache-2.0
//! `path`: well-known directories and lexical path arithmetic. Nothing here touches the disk, and
//! no permission is needed: the answers are strings, access to what they name is `fs`'s business.
use std::{
    ffi::OsString,
    path::{Component, Path, MAIN_SEPARATOR_STR},
};

use alef_core::{registry::dispatch::Registry, AlefError, ErrorCode};
use serde::Deserialize;

use crate::{json, ModuleContext};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct One {
    path: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Parts {
    parts: Vec<String>,
}

/// Collapses `.`, repeated separators and `..` without looking at the disk; the result uses the
/// platform separator, has no trailing separator and is `.` when nothing is left. `..` cannot climb
/// above a root, and is kept at the start of a relative path.
pub fn normalize(text: &str) -> String {
    let mut prefix = OsString::new();
    let mut rooted = false;
    let mut names: Vec<&std::ffi::OsStr> = Vec::new();
    for component in Path::new(text).components() {
        match component {
            Component::Prefix(value) => prefix = value.as_os_str().to_owned(),
            Component::RootDir => rooted = true,
            Component::CurDir => {}
            Component::ParentDir => match names.last() {
                Some(last) if *last != ".." => {
                    names.pop();
                }
                _ if rooted => {}
                _ => names.push(std::ffi::OsStr::new("..")),
            },
            Component::Normal(name) => names.push(name),
        }
    }
    let mut result = prefix.to_string_lossy().into_owned();
    if rooted {
        result.push_str(MAIN_SEPARATOR_STR);
    }
    let body: Vec<_> = names.iter().map(|name| name.to_string_lossy()).collect();
    result.push_str(&body.join(MAIN_SEPARATOR_STR));
    if result.is_empty() {
        ".".to_owned()
    } else {
        result
    }
}

/// Joins the non-empty parts with the separator, then normalizes (an absolute part does not reset
/// the path).
pub fn join(parts: &[String]) -> String {
    let joined: Vec<&str> = parts
        .iter()
        .map(String::as_str)
        .filter(|part| !part.is_empty())
        .collect();
    normalize(&joined.join(MAIN_SEPARATOR_STR))
}

/// The parent of the normalized path: `.` for a bare name, the root for a root.
pub fn dirname(text: &str) -> String {
    let normal = normalize(text);
    match Path::new(&normal).parent() {
        Some(parent) if parent.as_os_str().is_empty() => ".".to_owned(),
        Some(parent) => parent.to_string_lossy().into_owned(),
        None if normal == "." => ".".to_owned(),
        None => normal,
    }
}

/// The last name of the normalized path; empty for a root.
pub fn basename(text: &str) -> String {
    let normal = normalize(text);
    match Path::new(&normal).components().next_back() {
        Some(Component::Normal(name)) => name.to_string_lossy().into_owned(),
        Some(Component::ParentDir) => "..".to_owned(),
        _ => String::new(),
    }
}

pub(crate) fn register(registry: &mut Registry, context: &ModuleContext) -> Result<(), AlefError> {
    let paths = &context.paths;
    let directories = [
        ("path.appData", &paths.app_data),
        ("path.appConfig", &paths.app_config),
        ("path.appCache", &paths.app_cache),
        ("path.temp", &paths.temp),
        ("path.home", &paths.home),
        ("path.documents", &paths.documents),
        ("path.downloads", &paths.downloads),
        ("path.desktop", &paths.desktop),
    ];
    for (name, directory) in directories {
        let text = directory.to_string_lossy().into_owned();
        registry.command::<()>(name)?.handler(move |_ctx, ()| {
            let text = text.clone();
            async move { json(&text) }
        })?;
    }
    registry
        .command::<()>("path.executable")?
        .handler(|_ctx, ()| async move {
            let program = std::env::current_exe().map_err(|error| {
                AlefError::new(
                    ErrorCode::NotAvailable,
                    format!("no executable path: {error}"),
                )
            })?;
            json(&program.to_string_lossy())
        })?;
    registry
        .command::<Parts>("path.join")?
        .handler(|_ctx, args| async move { json(&join(&args.parts)) })?;
    registry
        .command::<One>("path.normalize")?
        .handler(|_ctx, args| async move { json(&normalize(&args.path)) })?;
    registry
        .command::<One>("path.dirname")?
        .handler(|_ctx, args| async move { json(&dirname(&args.path)) })?;
    registry
        .command::<One>("path.basename")?
        .handler(|_ctx, args| async move { json(&basename(&args.path)) })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Cases are written with `/` and turned into the platform spelling.
    fn native(text: &str) -> String {
        text.replace('/', MAIN_SEPARATOR_STR)
    }

    fn parts(items: &[&str]) -> Vec<String> {
        items.iter().map(|item| (*item).to_owned()).collect()
    }

    #[test]
    fn normalize_collapses_dots_and_separators_lexically() {
        for (input, expected) in [
            ("a/b/../c", "a/c"),
            ("a//b/./c/", "a/b/c"),
            ("./a", "a"),
            ("a/..", "."),
            ("", "."),
            (".", "."),
            ("../a", "../a"),
            ("../../a", "../../a"),
            ("a/../..", ".."),
            ("a/b/c/../../..", "."),
        ] {
            assert_eq!(normalize(input), native(expected), "{input:?}");
        }
    }

    #[cfg(unix)]
    #[test]
    fn unix_roots_absorb_parent_references() {
        for (input, expected) in [
            ("/", "/"),
            ("/..", "/"),
            ("/a/../..", "/"),
            ("/a/b/../c", "/a/c"),
            ("//a///b", "/a/b"),
            ("/a/b/", "/a/b"),
        ] {
            assert_eq!(normalize(input), expected, "{input:?}");
        }
        assert_eq!(dirname("/a/b"), "/a");
        assert_eq!(dirname("/a"), "/");
        assert_eq!(dirname("/"), "/");
        assert_eq!(basename("/"), "");
        assert_eq!(join(&parts(&["/usr", "/bin", "node"])), "/usr/bin/node");
        // a backslash is an ordinary character in a Unix file name
        assert_eq!(normalize(r"a\b/c"), r"a\b/c");
    }

    #[cfg(windows)]
    #[test]
    fn windows_prefixes_and_both_separators() {
        for (input, expected) in [
            (r"C:\a\..\b", r"C:\b"),
            ("C:/a//b/./c", r"C:\a\b\c"),
            (r"C:\", r"C:\"),
            (r"C:\..", r"C:\"),
            (r"C:a\..\b", r"C:b"),
            (r"\\server\share\x\..\y", r"\\server\share\y"),
            (r"a\b/c", r"a\b\c"),
            (r"\a", r"\a"),
        ] {
            assert_eq!(normalize(input), expected, "{input:?}");
        }
        assert_eq!(dirname(r"C:\a\b"), r"C:\a");
        assert_eq!(dirname(r"C:\a"), r"C:\");
        assert_eq!(dirname(r"C:\"), r"C:\");
        assert_eq!(basename(r"C:\"), "");
        assert_eq!(
            join(&parts(&["C:/Users", "me", "..", "you"])),
            r"C:\Users\you"
        );
    }

    #[test]
    fn join_concatenates_without_resetting_on_an_absolute_part() {
        assert_eq!(join(&parts(&["a", "b", "c.txt"])), native("a/b/c.txt"));
        assert_eq!(join(&parts(&["a", "", "b"])), native("a/b"));
        assert_eq!(join(&parts(&["a", "..", "b"])), "b");
        assert_eq!(join(&parts(&[])), ".");
        assert_eq!(join(&parts(&["", ""])), ".");
        assert_eq!(join(&parts(&["a/b", "../c"])), native("a/c"));
    }

    #[test]
    fn dirname_and_basename_work_on_the_normalized_path() {
        assert_eq!(dirname("a/b/c.txt"), native("a/b"));
        assert_eq!(dirname("c.txt"), ".");
        assert_eq!(dirname("."), ".");
        assert_eq!(dirname("a/b/"), "a");
        assert_eq!(dirname("a/b/.."), ".");
        assert_eq!(basename("a/b/c.txt"), "c.txt");
        assert_eq!(basename("a/b/"), "b");
        assert_eq!(basename("a/./b/.."), "a");
        assert_eq!(basename("."), "");
        assert_eq!(basename(".."), "..");
        assert_eq!(basename(""), "");
    }
}
