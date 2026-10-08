// SPDX-License-Identifier: MIT OR Apache-2.0
//! Which file a program text means: `sidecar:<name>` is `bin/<name>` in the folder of the
//! application and nothing else, any other text goes through the search path of the runtime.
use std::path::{Path, PathBuf};

use alef_core::{
    security::sidecar::{is_sidecar_reference, sidecar_name},
    AlefError, ErrorCode,
};

use super::{invalid, tree};

/// `bin/<name>` of the application; on Windows `bin/<name>.exe` when `bin/<name>` is not there.
/// Never searched for on `PATH`.
pub(crate) fn sidecar(app: &Path, name: &str) -> Result<PathBuf, AlefError> {
    let bin = app.join("bin");
    let plain = bin.join(name);
    let found = if plain.is_file() {
        Some(plain)
    } else if cfg!(windows) && !plain.exists() {
        Some(bin.join(format!("{name}.exe"))).filter(|exe| exe.is_file())
    } else {
        None
    };
    found.ok_or_else(|| {
        AlefError::new(
            ErrorCode::NotFound,
            format!("the sidecar {name} was not found"),
        )
    })
}

/// The program a text names. A text written as a sidecar reference is never a plain name: with a bad
/// name it is `INVALID_ARGUMENT`, with a good one the sidecar (or `NOT_FOUND`).
pub(crate) fn program(app: &Path, text: &str) -> Result<PathBuf, AlefError> {
    if is_sidecar_reference(text) {
        let name = sidecar_name(text).ok_or_else(|| invalid("invalid sidecar name"))?;
        return sidecar(app, name);
    }
    // Resolution uses the search path of the runtime, never the environment of the page.
    tree::resolve(
        text,
        std::env::var_os("PATH").as_deref(),
        std::env::var_os("PATHEXT").as_deref(),
    )
    .ok_or_else(|| {
        AlefError::new(
            ErrorCode::NotFound,
            format!("the program {text} was not found"),
        )
    })
}

/// The first word of a line a shell would run must not be a sidecar reference: a shell cannot
/// resolve it.
pub(crate) fn refuse_in_shell(first: Option<&str>) -> Result<(), AlefError> {
    match first {
        Some(word) if is_sidecar_reference(word) => {
            Err(invalid("a sidecar cannot be run through a shell"))
        }
        _ => Ok(()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sidecar_is_found_in_bin_of_the_application_and_nowhere_else() {
        let app = tempfile::tempdir().unwrap();
        assert_eq!(
            sidecar(app.path(), "tool").unwrap_err().code,
            ErrorCode::NotFound
        );
        std::fs::create_dir(app.path().join("bin")).unwrap();
        let file = app
            .path()
            .join("bin")
            .join(if cfg!(windows) { "tool.exe" } else { "tool" });
        std::fs::write(&file, b"x").unwrap();
        assert_eq!(sidecar(app.path(), "tool").unwrap(), file);
    }

    #[cfg(windows)]
    #[test]
    fn a_folder_with_the_name_of_a_sidecar_hides_its_exe() {
        let app = tempfile::tempdir().unwrap();
        let bin = app.path().join("bin");
        std::fs::create_dir_all(bin.join("tool")).unwrap();
        std::fs::write(bin.join("tool.exe"), b"x").unwrap();
        assert_eq!(
            sidecar(app.path(), "tool").unwrap_err().code,
            ErrorCode::NotFound
        );
    }

    #[test]
    fn a_bad_sidecar_reference_is_invalid_and_never_a_plain_name() {
        let app = tempfile::tempdir().unwrap();
        for text in ["sidecar:", "sidecar:..", "sidecar:a/b", "sidecar:a b"] {
            assert_eq!(
                program(app.path(), text).unwrap_err().code,
                ErrorCode::InvalidArgument,
                "{text}"
            );
        }
    }

    #[test]
    fn a_shell_refuses_a_sidecar_first_word() {
        assert!(refuse_in_shell(Some("sidecar:tool")).is_err());
        assert!(refuse_in_shell(Some("sidecar:")).is_err());
        assert!(refuse_in_shell(Some("tool")).is_ok());
        assert!(refuse_in_shell(None).is_ok());
    }
}
