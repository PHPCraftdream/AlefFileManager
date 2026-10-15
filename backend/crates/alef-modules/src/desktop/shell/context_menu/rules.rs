// SPDX-License-Identifier: MIT OR Apache-2.0
//! Pure rules for context-menu identifiers and paths.
use alef_core::{AlefError, ErrorCode};
use std::path::{Path, PathBuf};

const UNKNOWN: &str = "unknown menu item id";

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum InvokeSpec {
    Fallback(&'static str),
    OpenWith(String),
    WindowsOffset(u32),
}

/// User-visible ids are canonical fallbacks or platform command offsets only.
pub(crate) fn parse_id(id: &str) -> Result<InvokeSpec, AlefError> {
    let invalid = || AlefError::new(ErrorCode::InvalidArgument, UNKNOWN);
    match id {
        "open" => Ok(InvokeSpec::Fallback("open")),
        "reveal" => Ok(InvokeSpec::Fallback("reveal")),
        "trash" => Ok(InvokeSpec::Fallback("trash")),
        // Reject the dangerous token before validating the current menu item, on every OS.
        "verb:runas" | "runas" => Err(AlefError::new(
            ErrorCode::PermissionDenied,
            "shell.contextMenu: runas (elevation) is not allowed",
        )),
        value
            if value.starts_with("open-with:")
                && value.len() > 10
                && !value[10..].contains('\0') =>
        {
            Ok(InvokeSpec::OpenWith(value[10..].to_owned()))
        }
        value if value.starts_with("cmd:") => value[4..]
            .parse::<u32>()
            .map(InvokeSpec::WindowsOffset)
            .map_err(|_| invalid()),
        _ => Err(invalid()),
    }
}

pub(crate) fn verb_refused(
    verb: &str,
    paths: &[PathBuf],
    allow_runas: bool,
) -> Result<(), AlefError> {
    if verb.eq_ignore_ascii_case("runas") && !allow_runas {
        return Err(AlefError::new(
            ErrorCode::PermissionDenied,
            "shell.contextMenu: runas (elevation) is not allowed",
        ));
    }
    if (verb.eq_ignore_ascii_case("open") || verb.eq_ignore_ascii_case("open-with"))
        && paths
            .iter()
            .any(|path| super::super::opens_by_running(path))
    {
        return Err(AlefError::new(
            ErrorCode::PermissionDenied,
            "shell.openPath does not start programs",
        ));
    }
    Ok(())
}

pub(crate) fn same_parent(paths: &[PathBuf]) -> Result<(), AlefError> {
    const MESSAGE: &str = "shell.contextMenu: all paths must share one parent folder";
    if paths.is_empty() {
        return Err(AlefError::new(ErrorCode::InvalidArgument, MESSAGE));
    }
    let key = |path: &PathBuf| {
        path.parent().map(|parent| {
            parent
                .components()
                .map(|component| {
                    let value = component.as_os_str().to_string_lossy().into_owned();
                    if cfg!(windows) {
                        value.to_ascii_lowercase()
                    } else {
                        value
                    }
                })
                .collect::<Vec<_>>()
        })
    };
    let first =
        key(&paths[0]).ok_or_else(|| AlefError::new(ErrorCode::InvalidArgument, MESSAGE))?;
    if paths
        .iter()
        .skip(1)
        .any(|path| key(path).as_ref() != Some(&first))
    {
        return Err(AlefError::new(ErrorCode::InvalidArgument, MESSAGE));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn ids_use_the_menu_contract_without_trusting_verbs() {
        for (id, expected) in [
            ("open", InvokeSpec::Fallback("open")),
            ("reveal", InvokeSpec::Fallback("reveal")),
            ("trash", InvokeSpec::Fallback("trash")),
        ] {
            assert_eq!(parse_id(id).unwrap(), expected);
        }
        assert_eq!(
            parse_id("open-with:app-x").unwrap(),
            InvokeSpec::OpenWith("app-x".into())
        );
        assert_eq!(parse_id("cmd:12").unwrap(), InvokeSpec::WindowsOffset(12));
        for id in ["verb:open", "cmd:x", "open-with:", "junk"] {
            assert!(parse_id(id).is_err(), "{id}");
        }
        assert_eq!(
            parse_id("verb:runas").unwrap_err().code,
            ErrorCode::PermissionDenied
        );
    }

    #[test]
    fn open_refuses_all_running_file_types_even_in_a_mixed_selection() {
        let root = tempfile::tempdir().unwrap();
        for extension in ["exe", "bat", "ps1", "py", "lnk", "app", "desktop"] {
            let path = root
                .path()
                .join(format!("program.{}", extension.to_ascii_uppercase()));
            std::fs::write(&path, b"test").unwrap();
            assert_eq!(
                verb_refused("open", &[root.path().join("notes.txt"), path], false)
                    .unwrap_err()
                    .message,
                "shell.openPath does not start programs"
            );
        }
        assert!(verb_refused("open", &[root.path().join("notes.txt")], false).is_ok());
        assert!(verb_refused("open-with", &[root.path().join("notes.txt")], false).is_ok());
    }

    #[cfg(unix)]
    #[test]
    fn open_refuses_executable_files_without_a_dangerous_extension() {
        use std::os::unix::fs::PermissionsExt;
        let root = tempfile::tempdir().unwrap();
        let path = root.path().join("program.txt");
        std::fs::write(&path, b"test").unwrap();
        std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o755)).unwrap();
        assert_eq!(
            verb_refused("open", &[path], false).unwrap_err().message,
            "shell.openPath does not start programs"
        );
    }

    #[test]
    fn runas_allowance_is_case_insensitive() {
        assert_eq!(
            verb_refused("RuNaS", &[], false).unwrap_err().message,
            "shell.contextMenu: runas (elevation) is not allowed"
        );
        assert!(verb_refused("RuNaS", &[], true).is_ok());
    }

    #[test]
    fn parent_matching_rejects_different_parent_folders() {
        let root = tempfile::tempdir().unwrap();
        let a = root.path().join("a").join("one.txt");
        let b = root.path().join("b").join("two.txt");
        assert_eq!(
            same_parent(&[a, b]).unwrap_err().code,
            ErrorCode::InvalidArgument
        );
    }

    #[cfg(unix)]
    #[test]
    fn parent_matching_is_case_sensitive_on_unix() {
        let root = tempfile::tempdir().unwrap();
        let lower = root.path().join("parent").join("one.txt");
        let upper = root.path().join("Parent").join("two.txt");
        assert_eq!(
            same_parent(&[lower, upper]).unwrap_err().code,
            ErrorCode::InvalidArgument
        );
    }

    #[test]
    fn parent_matching_is_component_aware_and_empty_input_is_rejected() {
        let root = tempfile::tempdir().unwrap();
        let a = root.path().join("a");
        let b = root.path().join("b");
        same_parent(&[a.clone(), b]).expect("same parent");
        assert_eq!(
            same_parent(&[]).unwrap_err().code,
            ErrorCode::InvalidArgument
        );
    }
}
