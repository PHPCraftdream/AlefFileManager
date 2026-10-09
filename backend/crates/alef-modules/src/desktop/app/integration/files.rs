// SPDX-License-Identifier: MIT OR Apache-2.0
//! Exact-content ownership and atomic creation; never replace a foreign entry.
#![cfg_attr(windows, allow(dead_code))] // Unix encoders are also tested on Windows.
use super::{text, unavailable, Launch};
use alef_core::AlefError;
use std::{fs, io::Write, path::Path};

fn contents(path: &Path) -> Result<Option<Vec<u8>>, AlefError> {
    match fs::symlink_metadata(path) {
        Ok(meta) if !meta.file_type().is_file() => {
            Err(unavailable("integration entry is not a regular file"))
        }
        Ok(_) => fs::read(path).map(Some).map_err(unavailable),
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(unavailable(e)),
    }
}

pub(in crate::desktop::app) fn enabled(path: &Path, expected: &str) -> Result<bool, AlefError> {
    Ok(contents(path)?.is_some_and(|bytes| bytes == expected.as_bytes()))
}

pub(in crate::desktop::app) fn change(
    path: &Path,
    expected: &str,
    enable: bool,
) -> Result<(), AlefError> {
    if let Some(bytes) = contents(path)? {
        if bytes != expected.as_bytes() {
            return Err(unavailable(
                "entry belongs to another command; refusing to change it",
            ));
        }
        if !enable {
            match fs::remove_file(path) {
                Ok(()) => (),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => (),
                Err(e) => return Err(unavailable(e)),
            }
        }
        return Ok(());
    }
    if !enable {
        return Ok(());
    }
    let parent = path
        .parent()
        .ok_or_else(|| unavailable("entry has no parent"))?;
    fs::create_dir_all(parent).map_err(unavailable)?;
    let mut pending = tempfile::NamedTempFile::new_in(parent).map_err(unavailable)?;
    pending
        .write_all(expected.as_bytes())
        .map_err(unavailable)?;
    pending.as_file().sync_all().map_err(unavailable)?;
    match pending.persist_noclobber(path) {
        Ok(_) => Ok(()),
        Err(_e) if enabled(path, expected)? => Ok(()),
        Err(e) => Err(unavailable(e.error)),
    }
}

/// Desktop Entry string escaping is applied after Exec argument escaping (two parsing layers).
#[cfg(any(target_os = "linux", test))]
fn desktop_argument(value: &str) -> Result<String, AlefError> {
    text(value)?;
    let mut exec = String::from("\"");
    for ch in value.chars() {
        match ch {
            '%' => exec.push_str("%%"),
            '\\' | '"' | '`' | '$' => {
                exec.push('\\');
                exec.push(ch);
            }
            _ => exec.push(ch),
        }
    }
    exec.push('"');
    Ok(exec.replace('\\', "\\\\"))
}

#[cfg(any(target_os = "linux", test))]
pub(in crate::desktop::app) fn desktop(launch: &Launch) -> Result<String, AlefError> {
    let args = launch.arguments()?.map(desktop_argument);
    let [exe, flag, folder] = args;
    Ok(format!(
        "[Desktop Entry]\nType=Application\nName={}\nExec={} {} {}\nTerminal=false\n",
        launch.name, exe?, flag?, folder?
    ))
}
#[cfg(any(target_os = "macos", test))]
fn xml(value: &str) -> Result<String, AlefError> {
    text(value)?;
    Ok(value
        .replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&apos;"))
}
#[cfg(any(target_os = "macos", test))]
pub(in crate::desktop::app) fn plist(launch: &Launch) -> Result<String, AlefError> {
    let mut args = String::new();
    for arg in launch.arguments()? {
        args.push_str(&format!("<string>{}</string>", xml(arg)?));
    }
    Ok(format!("<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n<!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n<plist version=\"1.0\"><dict><key>Label</key><string>{}</string><key>ProgramArguments</key><array>{args}</array><key>RunAtLoad</key><true/></dict></plist>\n", launch.name))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn atomic_owned_entries_are_idempotent_and_foreign_entries_survive() {
        let scratch = tempfile::tempdir().unwrap();
        let path = scratch.path().join("autostart/test.desktop");
        change(&path, "owned", false).unwrap();
        assert!(!enabled(&path, "owned").unwrap());
        change(&path, "owned", true).unwrap();
        change(&path, "owned", true).unwrap();
        assert!(enabled(&path, "owned").unwrap());
        fs::write(&path, "foreign").unwrap();
        assert!(!enabled(&path, "owned").unwrap());
        assert!(change(&path, "owned", true).is_err());
        assert!(change(&path, "owned", false).is_err());
        assert_eq!(fs::read_to_string(&path).unwrap(), "foreign");
        fs::write(&path, "owned").unwrap();
        change(&path, "owned", false).unwrap();
        assert!(!path.exists());
        change(&path, "owned", false).unwrap();
    }
    #[test]
    fn platform_encodings_preserve_reserved_characters() {
        assert_eq!(
            desktop_argument("a% b\\\"$`").unwrap(),
            "\"a%% b\\\\\\\\\\\\\"\\\\$\\\\`\""
        );
        assert!(desktop_argument("a\nb").is_err());
        assert_eq!(xml("<&>\"'").unwrap(), "&lt;&amp;&gt;&quot;&apos;");
        let scratch = tempfile::tempdir().unwrap();
        let launch = Launch::resolve("org.test", scratch.path()).unwrap();
        let entry = desktop(&launch).unwrap();
        assert!(entry.starts_with("[Desktop Entry]\nType=Application\n"));
        assert!(entry.contains("\"--app\""));
        let entry = plist(&launch).unwrap();
        assert!(entry.contains("<key>RunAtLoad</key><true/>"));
        assert!(entry.contains("<string>--app</string>"));
        assert_eq!(entry.matches("<string>").count(), 4);
    }
}
