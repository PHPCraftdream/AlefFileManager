// SPDX-License-Identifier: MIT OR Apache-2.0
//! Autostart entry: executable plus argument vector, with validation and the
//! Windows ownership marker.

use std::ffi::{OsStr, OsString};
use std::path::PathBuf;

use super::AutostartError;

/// The constant argument marking the registry Run value as ours.
pub const MARKER_ARG: &str = "--alef-runtime-autostart";

/// One autostart record: a trusted executable and an argument array.
/// Never turned into a shell command line on POSIX; on Windows the run value
/// uses the documented `CommandLineToArgvW` quoting (`cmdline`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Entry {
    pub exe: PathBuf,
    pub args: Vec<OsString>,
}

impl Entry {
    pub fn new(exe: PathBuf, args: Vec<OsString>) -> Self {
        Self { exe, args }
    }

    /// Conservative validation: absolute exe, no NUL and no line breaks
    /// anywhere. Line breaks are rejected in arguments too, even though some
    /// formats could carry them: a record must render into every format.
    pub fn validate(&self) -> Result<(), AutostartError> {
        if !self.exe.is_absolute() {
            return Err(AutostartError::InvalidEntry(
                "executable path must be absolute",
            ));
        }
        clean_os_str(self.exe.as_os_str())?;
        for arg in &self.args {
            clean_os_str(arg)?;
        }
        Ok(())
    }

    /// Our record on Windows carries `MARKER_ARG` last; add it if absent.
    pub fn with_marker(&self) -> Self {
        if self.has_marker() {
            self.clone()
        } else {
            let mut args = self.args.clone();
            args.push(OsString::from(MARKER_ARG));
            Self {
                exe: self.exe.clone(),
                args,
            }
        }
    }

    pub fn has_marker(&self) -> bool {
        self.args
            .last()
            .is_some_and(|a| a.as_os_str() == OsStr::new(MARKER_ARG))
    }
}

fn clean_os_str(value: &OsStr) -> Result<(), AutostartError> {
    let bytes = value.as_encoded_bytes();
    if bytes.contains(&0) {
        return Err(AutostartError::InvalidEntry("NUL byte in autostart entry"));
    }
    if bytes.contains(&b'\n') || bytes.contains(&b'\r') {
        return Err(AutostartError::InvalidEntry(
            "line break in autostart entry",
        ));
    }
    if value.to_str().is_none() {
        return Err(AutostartError::InvalidEntry(
            "autostart entry must be valid UTF-8",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry(exe: &str, args: &[&str]) -> Entry {
        Entry::new(
            PathBuf::from(exe),
            args.iter().map(OsString::from).collect(),
        )
    }

    #[test]
    fn valid_absolute_entry_passes() {
        // tempdir-style paths are absolute on every platform.
        let exe = std::env::temp_dir().join("alef");
        assert!(Entry::new(exe, vec![OsString::from("--broker")])
            .validate()
            .is_ok());
        #[cfg(windows)]
        assert!(entry(r"C:\Program Files\Alef\alef.exe", &[])
            .validate()
            .is_ok());
        #[cfg(not(windows))]
        assert!(entry("/opt/alef/bin/alef", &[]).validate().is_ok());
    }

    #[test]
    fn relative_exe_is_rejected() {
        assert!(entry("alef", &[]).validate().is_err());
        assert!(entry("./alef", &[]).validate().is_err());
    }

    #[test]
    fn nul_in_exe_or_arg_is_rejected() {
        assert!(entry("/x/al\0ef", &[]).validate().is_err());
        assert!(entry("/x/alef", &["a\0b"]).validate().is_err());
    }

    #[test]
    fn line_breaks_are_rejected_everywhere() {
        assert!(entry("/x/al\nef", &[]).validate().is_err());
        assert!(entry("/x/al\ref", &[]).validate().is_err());
        assert!(entry("/x/alef", &["a\nb"]).validate().is_err());
        assert!(entry("/x/alef", &["a\rb"]).validate().is_err());
    }

    #[test]
    fn marker_is_added_once_and_detected() {
        let base = entry("/x/alef", &["run"]);
        assert!(!base.has_marker());
        let marked = base.with_marker();
        assert!(marked.has_marker());
        assert_eq!(marked.args.len(), 2);
        assert_eq!(marked.with_marker(), marked);
    }

    #[test]
    fn marker_position_is_last() {
        let marked = entry("/x/alef", &[]).with_marker();
        assert_eq!(marked.args, vec![OsString::from(MARKER_ARG)]);
        let explicit = entry("/x/alef", &["--alef-runtime-autostart"]);
        assert!(explicit.has_marker());
        assert_eq!(explicit.with_marker(), explicit);
    }
}
