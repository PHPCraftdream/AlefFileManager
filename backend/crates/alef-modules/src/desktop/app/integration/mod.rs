// SPDX-License-Identifier: MIT OR Apache-2.0
//! Shared native integration identity and launch command. Never captures process arguments or grants.
pub(super) mod deeplink_platform;
#[cfg(any(target_os = "linux", target_os = "macos", test))]
pub(super) mod files;
#[cfg(windows)]
pub(super) mod registry;

use alef_core::{AlefError, ErrorCode};
use std::path::{Path, PathBuf};

pub(super) fn unavailable(message: impl std::fmt::Display) -> AlefError {
    AlefError::new(
        ErrorCode::NotAvailable,
        format!("app integration: {message}"),
    )
}

pub(super) fn text(value: &str) -> Result<&str, AlefError> {
    if value.is_empty() || value.chars().any(char::is_control) {
        return Err(unavailable("empty text or control character"));
    }
    Ok(value)
}

/// Hex is reversible, case-insensitive-filesystem safe and cannot inject a path separator.
pub(super) fn safe_id(id: &str) -> Result<String, AlefError> {
    text(id)?;
    if id.len() > 100 {
        return Err(unavailable("application identity exceeds 100 UTF-8 bytes"));
    }
    let mut result = String::from("alef-");
    use std::fmt::Write;
    for byte in id.bytes() {
        write!(&mut result, "{byte:02x}").map_err(unavailable)?;
    }
    Ok(result)
}

pub(super) fn path_text(path: &Path) -> Result<&str, AlefError> {
    text(
        path.to_str()
            .ok_or_else(|| unavailable("path is not Unicode"))?,
    )
}

#[derive(Debug)]
pub(super) struct Launch {
    pub name: String,
    pub exe: PathBuf,
    pub folder: PathBuf,
}
impl Launch {
    pub fn resolve(id: &str, folder: &Path) -> Result<Self, AlefError> {
        let name = safe_id(id)?;
        path_text(folder)?;
        if !folder.is_absolute() {
            return Err(unavailable("application folder must be absolute"));
        }
        let folder = folder.canonicalize().map_err(unavailable)?;
        if !folder.is_dir() {
            return Err(unavailable("application folder is not a directory"));
        }
        let exe = std::env::current_exe()
            .map_err(unavailable)?
            .canonicalize()
            .map_err(unavailable)?;
        #[cfg(windows)]
        let (exe, folder) = (shell_path(&exe)?, shell_path(&folder)?);
        path_text(&exe)?;
        path_text(&folder)?;
        Ok(Self { name, exe, folder })
    }
    #[cfg(windows)]
    pub fn windows_command(&self) -> Result<String, AlefError> {
        Ok(self.arguments()?.map(windows_quote).join(" "))
    }
    pub fn arguments(&self) -> Result<[&str; 3], AlefError> {
        Ok([path_text(&self.exe)?, "--app", path_text(&self.folder)?])
    }
}

/// Shell registrations use ordinary drive/UNC paths, not canonicalization's verbatim prefix.
#[cfg(windows)]
fn shell_path(path: &Path) -> Result<PathBuf, AlefError> {
    let value = path_text(path)?;
    let ordinary = if let Some(unc) = value.strip_prefix(r"\\?\UNC\") {
        format!(r"\\{unc}")
    } else if let Some(drive) = value.strip_prefix(r"\\?\") {
        let bytes = drive.as_bytes();
        if bytes.len() < 3 || !bytes[0].is_ascii_alphabetic() || &bytes[1..3] != b":\\" {
            return Err(unavailable("unsupported verbatim shell path"));
        }
        drive.to_owned()
    } else {
        value.to_owned()
    };
    Ok(PathBuf::from(ordinary))
}

/// Windows CommandLineToArgvW/CRT quoting, including trailing backslashes.
#[cfg(any(windows, test))]
pub(super) fn windows_quote(value: &str) -> String {
    let mut out = String::from("\"");
    let mut slashes = 0;
    for ch in value.chars() {
        if ch == '\\' {
            slashes += 1;
            continue;
        }
        out.extend(std::iter::repeat_n(
            '\\',
            if ch == '"' { slashes * 2 + 1 } else { slashes },
        ));
        slashes = 0;
        out.push(ch);
    }
    out.extend(std::iter::repeat_n('\\', slashes * 2));
    out.push('"');
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn names_are_reversible_and_commands_quote_boundaries() {
        assert_eq!(safe_id("A/é").unwrap(), "alef-412fc3a9");
        assert_ne!(safe_id("A").unwrap(), safe_id("a").unwrap());
        assert!(safe_id("bad\nidentity").is_err());
        assert!(safe_id("").is_err());
        assert!(safe_id(&"x".repeat(101)).is_err());
        assert_eq!(windows_quote("a b\\"), "\"a b\\\\\"");
        assert_eq!(windows_quote("a\"b"), "\"a\\\"b\"");
    }
    #[test]
    fn canonical_launch_only_has_the_mandated_arguments() {
        let scratch = tempfile::tempdir().unwrap();
        let launch = Launch::resolve("test", scratch.path()).unwrap();
        assert_eq!(launch.arguments().unwrap()[1], "--app");
        #[cfg(not(windows))]
        assert_eq!(launch.folder, scratch.path().canonicalize().unwrap());
        #[cfg(windows)]
        assert_eq!(
            launch.folder,
            shell_path(&scratch.path().canonicalize().unwrap()).unwrap()
        );
        assert!(Launch::resolve("test", Path::new("relative")).is_err());
        assert!(Launch::resolve("test", Path::new(".")).is_err());
        assert!(text("bad\0value").is_err());
    }
    #[cfg(windows)]
    #[test]
    fn native_parser_round_trips_the_registered_command_without_registry_calls() {
        #[link(name = "shell32")]
        extern "system" {
            fn CommandLineToArgvW(command: *const u16, count: *mut i32) -> *mut *mut u16;
        }
        #[link(name = "kernel32")]
        extern "system" {
            fn LocalFree(memory: *mut std::ffi::c_void) -> *mut std::ffi::c_void;
        }
        let parse = |command: &str| {
            let wide: Vec<u16> = command.encode_utf16().chain(Some(0)).collect();
            let mut count = 0;
            // SAFETY: terminated input and writable count; returned allocation is read then freed once.
            unsafe {
                let argv = CommandLineToArgvW(wide.as_ptr(), &mut count);
                assert!(!argv.is_null());
                let result: Vec<String> = std::slice::from_raw_parts(argv, count as usize)
                    .iter()
                    .map(|&arg| {
                        let mut len = 0;
                        while *arg.add(len) != 0 {
                            len += 1;
                        }
                        String::from_utf16(std::slice::from_raw_parts(arg, len)).unwrap()
                    })
                    .collect();
                LocalFree(argv.cast());
                result
            }
        };
        let scratch = tempfile::tempdir().unwrap();
        let launch = Launch::resolve("test", scratch.path()).unwrap();
        assert!(!path_text(&launch.exe).unwrap().starts_with(r"\\?\"));
        assert_eq!(
            parse(&launch.windows_command().unwrap()),
            launch.arguments().unwrap()
        );
        let launch = Launch {
            name: "test".into(),
            exe: PathBuf::from(r"C:\Program Files\Alef\alef.exe"),
            folder: PathBuf::from(r"C:\app folder\"),
        };
        assert_eq!(
            parse(&launch.windows_command().unwrap()),
            launch.arguments().unwrap()
        );
        assert_eq!(
            parse(&format!("alef.exe {}", windows_quote("a\"b\\"))),
            ["alef.exe", "a\"b\\"]
        );
        assert_eq!(
            shell_path(Path::new(r"\\?\UNC\server\share\app")).unwrap(),
            PathBuf::from(r"\\server\share\app")
        );
        assert!(shell_path(Path::new(r"\\?\Volume{invalid}\app")).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn non_unicode_paths_are_rejected() {
        use std::os::unix::ffi::OsStringExt;
        let path = PathBuf::from(std::ffi::OsString::from_vec(vec![255]));
        assert!(path_text(&path).is_err());
    }
}
