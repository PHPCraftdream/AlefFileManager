// SPDX-License-Identifier: MIT OR Apache-2.0
//! Platform registration. No native calls in formatting/ownership tests.
#[cfg(not(windows))]
use super::unavailable;
use super::Launch;
#[cfg(any(windows, target_os = "linux", test))]
use super::{self as integration};
use alef_core::AlefError;

pub(in crate::desktop::app) fn apply(
    launch: &Launch,
    schemes: &[String],
    register: bool,
) -> Result<(), AlefError> {
    #[cfg(windows)]
    {
        integration::registry::deep_links(launch, schemes, register)
    }
    #[cfg(target_os = "linux")]
    {
        linux::apply(launch, schemes, register)
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        let _ = (launch, schemes, register);
        Err(unavailable(
            "deep links require an M7 signed application bundle on macOS; unsupported elsewhere",
        ))
    }
}

#[cfg(any(target_os = "linux", test))]
fn desktop(launch: &Launch, schemes: &[String]) -> Result<String, AlefError> {
    let base = integration::files::desktop(launch)?;
    let base = base.replace("\nTerminal=false", " %u\nTerminal=false");
    Ok(format!(
        "{base}MimeType={}\n",
        schemes
            .iter()
            .map(|s| format!("x-scheme-handler/{s};"))
            .collect::<String>()
    ))
}

/// `old` without the lines that make `name` the default handler of `mime`; no other line changes.
#[cfg(any(target_os = "linux", test))]
fn without_default(old: &str, mime: &str, name: &str) -> String {
    let mut section = false;
    let mut new = String::new();
    for raw in old.split_inclusive('\n') {
        let line = raw.trim_end_matches(['\r', '\n']);
        if line.starts_with('[') {
            section = line == "[Default Applications]";
        }
        if section && (line == format!("{mime}={name}") || line == format!("{mime}={name};")) {
            continue;
        }
        new.push_str(raw);
    }
    new
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use std::{
        fs,
        path::PathBuf,
        process::{Command, Stdio},
        time::{Duration, Instant},
    };

    // Redirect stdout to a file rather than a pipe: a noisy child cannot deadlock the wait.
    // Only this owned child handle is killed. Every wait, including post-kill reaping, is bounded.
    fn xdg(args: &[&str]) -> Result<String, AlefError> {
        let output = tempfile::tempfile().map_err(unavailable)?;
        let mut child = Command::new("xdg-mime")
            .args(args)
            .stdin(Stdio::null())
            .stdout(output.try_clone().map_err(unavailable)?)
            .stderr(Stdio::null())
            .spawn()
            .map_err(unavailable)?;
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            if let Some(status) = child.try_wait().map_err(unavailable)? {
                if !status.success() {
                    return Err(unavailable("xdg-mime failed"));
                }
                break;
            }
            if Instant::now() >= deadline {
                let _ = child.kill();
                let reap = Instant::now() + Duration::from_secs(1);
                while Instant::now() < reap {
                    if child.try_wait().map_err(unavailable)?.is_some() {
                        break;
                    }
                    std::thread::sleep(Duration::from_millis(10));
                }
                return Err(unavailable("xdg-mime timed out"));
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        use std::io::{Read, Seek};
        if output.metadata().map_err(unavailable)?.len() > 8192 {
            return Err(unavailable("xdg-mime output too large"));
        }
        let mut output = output;
        output.rewind().map_err(unavailable)?;
        let mut result = String::new();
        output
            .take(8193)
            .read_to_string(&mut result)
            .map_err(unavailable)?;
        Ok(result.trim().to_owned())
    }

    pub(super) fn apply(
        launch: &Launch,
        schemes: &[String],
        register: bool,
    ) -> Result<(), AlefError> {
        let home =
            PathBuf::from(std::env::var_os("HOME").ok_or_else(|| unavailable("HOME is not set"))?);
        if !home.is_absolute() {
            return Err(unavailable("HOME must be absolute"));
        }
        integration::path_text(&home)?;
        let root = home.join(".local/share/applications");
        let name = format!("{}.desktop", launch.name);
        let path = root.join(&name);
        // Each subset uses the same full identity entry; changing a declaration set is deliberately
        // refused rather than overwriting an entry whose ownership cannot be established exactly.
        let expected = desktop(launch, schemes)?;
        if register {
            integration::files::change(&path, &expected, true)?;
        } else if !integration::files::enabled(&path, &expected)? {
            if std::fs::symlink_metadata(&path).is_ok() {
                return Err(unavailable("foreign desktop entry"));
            }
            return Ok(());
        }
        for scheme in schemes {
            let mime = format!("x-scheme-handler/{scheme}");
            let backup = root.join(format!("{}.{}.previous", launch.name, scheme));
            let current = xdg(&["query", "default", &mime])?;
            if register {
                if current != name {
                    // An existing backup is never overwritten; exact contents tie it to this launch.
                    let saved = format!("{expected}\nPrevious={current}\n");
                    integration::files::change(&backup, &saved, true)?;
                    xdg(&["default", &name, &mime])?;
                }
            } else if current == name {
                let bytes = fs::read_to_string(&backup).map_err(unavailable)?;
                let previous = bytes
                    .strip_prefix(&format!("{expected}\nPrevious="))
                    .and_then(|s| s.strip_suffix('\n'))
                    .ok_or_else(|| unavailable("foreign default backup"))?;
                if previous.is_empty() {
                    // Remove only our own default from the user config; preserve all foreign lines.
                    let config = std::env::var_os("XDG_CONFIG_HOME")
                        .map(PathBuf::from)
                        .unwrap_or_else(|| home.join(".config"));
                    if !config.is_absolute() {
                        return Err(unavailable("XDG_CONFIG_HOME must be absolute"));
                    }
                    let defaults = config.join("mimeapps.list");
                    if !fs::symlink_metadata(&defaults)
                        .map_err(unavailable)?
                        .file_type()
                        .is_file()
                    {
                        return Err(unavailable("mimeapps.list is not a regular file"));
                    }
                    let old = fs::read_to_string(&defaults).map_err(unavailable)?;
                    let new = super::without_default(&old, &mime, &name);
                    if new == old {
                        return Err(unavailable("owned default was not in user mimeapps.list"));
                    }
                    use std::io::Write;
                    let mut pending =
                        tempfile::NamedTempFile::new_in(&config).map_err(unavailable)?;
                    pending.write_all(new.as_bytes()).map_err(unavailable)?;
                    pending.as_file().sync_all().map_err(unavailable)?;
                    if fs::read_to_string(&defaults).map_err(unavailable)? != old {
                        return Err(unavailable("mimeapps.list changed before replacement"));
                    }
                    pending.persist(&defaults).map_err(unavailable)?;
                } else {
                    integration::text(previous)?;
                    xdg(&["default", previous, &mime])?;
                }
                integration::files::change(&backup, &bytes, false)?;
            }
        }
        if !register {
            integration::files::change(&path, &expected, false)?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn desktop_has_url_placeholder_and_declared_mime_types_only() {
        let scratch = tempfile::tempdir().unwrap();
        let launch = Launch::resolve("org.example.deep", scratch.path()).unwrap();
        let entry = desktop(&launch, &["sample".into(), "other".into()]).unwrap();
        assert!(entry.contains(" %u\n"));
        assert!(entry.contains("MimeType=x-scheme-handler/sample;x-scheme-handler/other;\n"));
        let path = scratch.path().join("entry.desktop");
        integration::files::change(&path, &entry, true).unwrap();
        assert!(integration::files::change(&path, "foreign", false).is_err());
        assert!(integration::files::enabled(&path, &entry).unwrap());
    }

    #[test]
    fn only_our_own_default_leaves_the_user_mimeapps_list() {
        let mime = "x-scheme-handler/a";
        let old = "[Added Associations]\nx-scheme-handler/a=n.desktop;\n[Default Applications]\nx-scheme-handler/a=n.desktop\r\nx-scheme-handler/b=n.desktop\nx-scheme-handler/a=n.desktop;\nx-scheme-handler/a=other.desktop\n[Removed Associations]\nx-scheme-handler/a=n.desktop\n";
        assert_eq!(
            without_default(old, mime, "n.desktop"),
            "[Added Associations]\nx-scheme-handler/a=n.desktop;\n[Default Applications]\nx-scheme-handler/b=n.desktop\nx-scheme-handler/a=other.desktop\n[Removed Associations]\nx-scheme-handler/a=n.desktop\n"
        );
        assert_eq!(without_default(old, mime, "none.desktop"), old);
        assert_eq!(without_default("", mime, "n.desktop"), "");
    }
}
