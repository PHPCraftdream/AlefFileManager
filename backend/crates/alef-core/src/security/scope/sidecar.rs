// SPDX-License-Identifier: MIT OR Apache-2.0
//! Sidecar programs: `sidecar:<name>` names the program `bin/<name>` in the folder of the
//! application, never a program found on `PATH`.
use super::path::windows_unsafe;

/// Prefix of a sidecar reference; a program that starts with it is never a plain program.
pub const SIDECAR_PREFIX: &str = "sidecar:";

/// `[A-Za-z0-9][A-Za-z0-9._-]*`: no separators, never `.` or `..`; on Windows also not a device or
/// alias name.
pub fn valid_sidecar_name(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_alphanumeric())
        && chars.all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '_' | '-'))
        && !windows_unsafe(name)
}

/// Whether `program` is written as a sidecar reference, valid or not.
pub fn is_sidecar_reference(program: &str) -> bool {
    program.starts_with(SIDECAR_PREFIX)
}

/// The name of a valid reference `sidecar:<name>`; `None` for anything else, including a
/// reference with a bad name (check [`is_sidecar_reference`] to tell it from a plain program).
pub fn sidecar_name(program: &str) -> Option<&str> {
    program
        .strip_prefix(SIDECAR_PREFIX)
        .filter(|name| valid_sidecar_name(name))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_sidecar_name_is_one_plain_file_name() {
        for good in ["tool", "Tool_2", "a", "7z", "my-tool.v2", "a..b", "x-"] {
            assert!(valid_sidecar_name(good), "{good}");
        }
        for bad in [
            "", ".", "..", ".hidden", "-x", "_x", "a/b", "a\\b", "/a", "a b", "a:b", "é", "a\0b",
            "a\n", "*",
        ] {
            assert!(!valid_sidecar_name(bad), "{bad:?}");
        }
        assert_eq!(valid_sidecar_name("con"), !cfg!(windows));
        assert_eq!(valid_sidecar_name("nul.txt"), !cfg!(windows));
    }

    #[test]
    fn a_reference_is_split_into_the_prefix_and_a_valid_name() {
        assert_eq!(sidecar_name("sidecar:tool"), Some("tool"));
        assert_eq!(sidecar_name("sidecar:"), None);
        assert_eq!(sidecar_name("sidecar:.."), None);
        assert_eq!(sidecar_name("sidecar:a/b"), None);
        assert_eq!(sidecar_name("tool"), None);
        assert_eq!(sidecar_name("Sidecar:tool"), None);
        assert!(is_sidecar_reference("sidecar:a/b"));
        assert!(is_sidecar_reference("sidecar:"));
        assert!(!is_sidecar_reference("tool"));
        assert!(!is_sidecar_reference("Sidecar:tool"));
    }
}
