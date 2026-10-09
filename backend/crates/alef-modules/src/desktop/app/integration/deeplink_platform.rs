// SPDX-License-Identifier: MIT OR Apache-2.0
//! Platform registration. No native calls in formatting/ownership tests.
#[cfg(any(not(windows), test))]
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

/// Takes the association of `name` out of those of `files` that exist; every other line stays.
/// Whether any file held it.
#[cfg(any(target_os = "linux", test))]
fn forget_default(files: &[std::path::PathBuf], mime: &str, name: &str) -> Result<bool, AlefError> {
    use std::{fs, io::Write};
    let mut forgotten = false;
    for file in files {
        match fs::symlink_metadata(file) {
            Ok(meta) if meta.file_type().is_file() => (),
            Ok(_) => return Err(unavailable("a file of default applications is not regular")),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => continue,
            Err(e) => return Err(unavailable(e)),
        }
        let old = fs::read_to_string(file).map_err(unavailable)?;
        let new = without_default(&old, mime, name);
        if new == old {
            continue;
        }
        let parent = file
            .parent()
            .ok_or_else(|| unavailable("file of default applications has no parent"))?;
        let mut pending = tempfile::NamedTempFile::new_in(parent).map_err(unavailable)?;
        pending.write_all(new.as_bytes()).map_err(unavailable)?;
        pending.as_file().sync_all().map_err(unavailable)?;
        pending.persist(file).map_err(unavailable)?;
        forgotten = true;
    }
    Ok(forgotten)
}

#[cfg(target_os = "linux")]
mod linux {
    use super::*;
    use std::{
        fs,
        path::{Path, PathBuf},
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

    /// The files of the user where `xdg-mime default` may have written an association: which one
    /// it takes depends on the version of xdg-utils and on the desktop.
    fn default_files(home: &Path) -> Result<Vec<PathBuf>, AlefError> {
        let place = |variable: &str, fallback: &str| -> Result<PathBuf, AlefError> {
            let path = std::env::var_os(variable)
                .filter(|value| !value.is_empty())
                .map_or_else(|| home.join(fallback), PathBuf::from);
            if path.is_absolute() {
                Ok(path)
            } else {
                Err(unavailable(format!("{variable} must be absolute")))
            }
        };
        let config = place("XDG_CONFIG_HOME", ".config")?;
        let data = place("XDG_DATA_HOME", ".local/share")?;
        Ok(vec![
            config.join("mimeapps.list"),
            data.join("applications/mimeapps.list"),
            data.join("applications/defaults.list"),
        ])
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
        let default_of =
            |scheme: &String| xdg(&["query", "default", &format!("x-scheme-handler/{scheme}")]);
        // The entry declares the schemes, so once it exists it answers for the default by itself:
        // what was the default before has to be asked first.
        let mut before = Vec::new();
        if register {
            for scheme in schemes {
                before.push(default_of(scheme)?);
            }
            integration::files::change(&path, &expected, true)?;
        } else if !integration::files::enabled(&path, &expected)? {
            if std::fs::symlink_metadata(&path).is_ok() {
                return Err(unavailable("foreign desktop entry"));
            }
            return Ok(());
        }
        let head = format!("{expected}\nPrevious=");
        for (index, scheme) in schemes.iter().enumerate() {
            let mime = format!("x-scheme-handler/{scheme}");
            let backup = root.join(format!("{}.{}.previous", launch.name, scheme));
            let saved = match fs::read_to_string(&backup) {
                Ok(text) if text.starts_with(&head) => Some(text),
                Ok(_) => return Err(unavailable("foreign default backup")),
                Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
                Err(e) => return Err(unavailable(e)),
            };
            if register {
                if saved.is_none() {
                    let previous: &str = if before[index] == name {
                        ""
                    } else {
                        &before[index]
                    };
                    integration::files::change(&backup, &format!("{head}{previous}\n"), true)?;
                    xdg(&["default", &name, &mime])?;
                }
            } else if let Some(saved) = saved {
                let previous = saved
                    .strip_prefix(&head)
                    .and_then(|s| s.strip_suffix('\n'))
                    .ok_or_else(|| unavailable("foreign default backup"))?;
                // A default the user has changed since is theirs and stays.
                if default_of(scheme)? == name {
                    if previous.is_empty() {
                        // Nothing to take out when the answer was only our entry declaring it.
                        super::forget_default(&default_files(&home)?, &mime, &name)?;
                    } else {
                        integration::text(previous)?;
                        xdg(&["default", previous, &mime])?;
                    }
                }
                integration::files::change(&backup, &saved, false)?;
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

    #[test]
    fn our_default_is_taken_from_whichever_file_of_the_user_holds_it() {
        let scratch = tempfile::tempdir().unwrap();
        let mime = "x-scheme-handler/a";
        let own =
            "[Default Applications]\nx-scheme-handler/a=n.desktop\nx-scheme-handler/b=o.desktop\n";
        let kept = "[Default Applications]\nx-scheme-handler/b=o.desktop\n";
        let first = scratch.path().join("mimeapps.list");
        let second = scratch.path().join("legacy/mimeapps.list");
        let absent = scratch.path().join("absent.list");
        std::fs::create_dir(second.parent().unwrap()).unwrap();
        std::fs::write(&second, own).unwrap();
        assert!(forget_default(
            &[first.clone(), second.clone(), absent.clone()],
            mime,
            "n.desktop",
        )
        .unwrap());
        assert_eq!(std::fs::read_to_string(&second).unwrap(), kept);
        assert!(!first.exists() && !absent.exists());
        std::fs::write(&first, own).unwrap();
        std::fs::write(&second, own).unwrap();
        assert!(forget_default(&[first.clone(), second.clone()], mime, "n.desktop").unwrap());
        assert_eq!(std::fs::read_to_string(&first).unwrap(), kept);
        assert_eq!(std::fs::read_to_string(&second).unwrap(), kept);
        assert!(!forget_default(&[first.clone(), absent], mime, "n.desktop").unwrap());
        std::fs::write(&first, own).unwrap();
        let directory = scratch.path().to_path_buf();
        assert!(forget_default(&[directory, first.clone()], mime, "n.desktop").is_err());
        assert_eq!(std::fs::read_to_string(&first).unwrap(), own);
    }
}
