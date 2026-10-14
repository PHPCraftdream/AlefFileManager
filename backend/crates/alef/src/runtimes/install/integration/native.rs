// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native desktop integration artifacts.
#[cfg(target_os = "linux")]
use super::desktop_entry;
#[cfg(any(target_os = "macos", windows))]
use super::safe_display_component;
#[cfg(windows)]
use super::windows_arguments;
#[cfg(target_os = "macos")]
use super::{mac_launcher, plist};
use super::{Artifact, Integration};
use std::{
    io,
    path::{Path, PathBuf},
};

pub(crate) struct NativeIntegration;
impl NativeIntegration {
    pub(crate) fn new() -> Self {
        Self
    }
    fn directory() -> io::Result<PathBuf> {
        #[cfg(any(test, feature = "e2e"))]
        if let Some(path) = std::env::var_os("ALEF_APPLICATIONS_DIR") {
            let path = PathBuf::from(path);
            if !path.is_absolute() {
                return Err(invalid("applications directory must be absolute"));
            }
            return Ok(path);
        }
        #[cfg(target_os = "linux")]
        {
            return dirs::data_dir()
                .map(|p| p.join("applications"))
                .ok_or_else(|| invalid("data directory unavailable"));
        }
        #[cfg(target_os = "macos")]
        {
            return dirs::home_dir()
                .map(|p| p.join("Applications"))
                .ok_or_else(|| invalid("home directory unavailable"));
        }
        #[cfg(windows)]
        {
            return std::env::var_os("APPDATA")
                .map(PathBuf::from)
                .map(|p| p.join("Microsoft/Windows/Start Menu/Programs"))
                .ok_or_else(|| invalid("APPDATA unavailable"));
        }
        #[allow(unreachable_code)]
        Err(io::Error::new(
            io::ErrorKind::Unsupported,
            "native integration unsupported",
        ))
    }
}
fn invalid(s: &'static str) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, s)
}
fn check_dir(p: &Path) -> io::Result<()> {
    crate::install::store::create_checked_dir(p)
}
fn plain_file(p: &Path) -> io::Result<bool> {
    match std::fs::symlink_metadata(p) {
        Ok(_) if crate::runtimes::store::is_regular_file(p) => Ok(true),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "artifact is linked or not a file",
        )),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(e),
    }
}
#[cfg(any(target_os = "linux", windows))]
fn remove_plain(path: &Path) -> io::Result<bool> {
    if plain_file(path)? {
        std::fs::remove_file(path)?;
        Ok(true)
    } else {
        Ok(false)
    }
}
#[cfg(windows)]
struct StageGuard(Option<PathBuf>);
#[cfg(windows)]
impl Drop for StageGuard {
    fn drop(&mut self) {
        if let Some(path) = &self.0 {
            if let Err(error) = super::transaction::remove_artifact(path) {
                eprintln!(
                    "alef: APP_STORE: shortcut stage cleanup failed at {}: {error}",
                    path.display()
                );
            }
        }
    }
}
#[cfg(any(windows, target_os = "macos"))]
fn with_cleanup(error: io::Error, cleanup: io::Result<()>) -> io::Error {
    match cleanup {
        Ok(()) => error,
        Err(cleanup) => io::Error::other(format!("{error}; stage cleanup failed: {cleanup}")),
    }
}
impl Integration for NativeIntegration {
    fn install(&mut self, a: &Artifact) -> io::Result<()> {
        if !crate::install::valid_id(&a.id)
            || a.hash.len() != 64
            || !a
                .hash
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
            || a.hash != sha256_id(&a.id)
            || !a.executable.is_absolute()
            || !a.package.is_absolute()
        {
            return Err(invalid("unsafe application identity"));
        }
        let root = Self::directory()?;
        #[cfg(target_os = "linux")]
        check_dir(&root)?;
        #[cfg(any(target_os = "macos", windows))]
        let filename = safe_display_component(&a.name, &a.hash);
        #[cfg(target_os = "linux")]
        {
            check_dir(&root)?;
            let dest = root.join(format!("alef-{}.desktop", a.hash));
            let prior = if plain_file(&dest)? {
                Some(std::fs::read(&dest)?)
            } else {
                None
            };
            let text = desktop_entry(a)?;
            let result = if prior.is_some() {
                crate::install::store::replace_record(&root, &dest, text.as_bytes())
            } else {
                crate::install::store::publish_bytes_no_replace(&root, &dest, text.as_bytes())
            };
            if let Err(error) = result {
                if let Some(bytes) = prior {
                    let _ = crate::install::store::replace_record(&root, &dest, &bytes);
                }
                return Err(error);
            }
            Ok(())
        }
        #[cfg(target_os = "macos")]
        {
            check_dir(&root)?;
            mac_install(&root, &filename, a)
        }
        #[cfg(windows)]
        {
            check_dir(&root)?;
            windows_install(&root, &filename, a)
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
        {
            let _ = (root, a);
            Err(io::Error::new(
                io::ErrorKind::Unsupported,
                "native integration unsupported",
            ))
        }
    }
    fn remove(&mut self, id: &str, name: Option<&str>) -> io::Result<bool> {
        if !crate::install::valid_id(id) {
            return Err(invalid("unsafe application id"));
        }
        let root = Self::directory()?;
        if !crate::runtimes::store::is_plain_directory(&root) {
            return match std::fs::symlink_metadata(&root) {
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
                Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
                _ => Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "unsafe applications directory",
                )),
            };
        }
        let h = sha256_id(id);
        let mut removed = false;
        #[cfg(target_os = "linux")]
        {
            let _ = name;
            removed |= remove_plain(&root.join(format!("alef-{h}.desktop")))?;
        }
        #[cfg(target_os = "macos")]
        {
            let _ = name;
            let suffix = format!("~{h}.app");
            for entry in std::fs::read_dir(&root)? {
                let path = entry?.path();
                if path
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.ends_with(&suffix))
                {
                    if !crate::runtimes::store::is_plain_directory(&path) {
                        return Err(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "unsafe owned bundle",
                        ));
                    }
                    validate_bundle(&path)?;
                    remove_bundle_safe(&path)?;
                    removed = true;
                }
            }
        }
        #[cfg(windows)]
        {
            removed = windows_remove(&root, id, name)?;
        }
        Ok(removed)
    }
}
#[cfg(any(target_os = "macos", test))]
fn validate_bundle(path: &Path) -> io::Result<()> {
    fn check(path: &Path, depth: usize) -> io::Result<()> {
        if depth > 64 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "bundle nesting too deep",
            ));
        }
        for entry in std::fs::read_dir(path)? {
            let entry = entry?;
            let m = std::fs::symlink_metadata(entry.path())?;
            if m.file_type().is_symlink() || !(m.is_dir() || m.is_file()) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "special or linked bundle entry",
                ));
            }
            if m.is_dir() {
                check(&entry.path(), depth + 1)?;
            }
        }
        Ok(())
    }
    check(path, 0)
}
#[cfg(target_os = "macos")]
fn remove_bundle_safe(path: &Path) -> io::Result<()> {
    validate_bundle(path)?;
    std::fs::remove_dir_all(path)
}
#[cfg(target_os = "macos")]
fn write_new(path: &Path, bytes: &[u8]) -> io::Result<()> {
    use std::io::Write;
    let mut f = std::fs::OpenOptions::new()
        .write(true)
        .create_new(true)
        .open(path)?;
    f.write_all(bytes)?;
    f.sync_all()
}
fn sha256_id(id: &str) -> String {
    use sha2::{Digest, Sha256};
    format!("{:x}", Sha256::digest(id.as_bytes()))
}
#[cfg(target_os = "macos")]
fn mac_install(root: &Path, filename: &str, a: &Artifact) -> io::Result<()> {
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    let final_path = root.join(format!("{filename}.app"));
    let stage = loop {
        let candidate = root.join(format!(
            ".alef-stage-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        match std::fs::create_dir(&candidate) {
            Ok(()) => break candidate,
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    };
    let contents = stage.join("Contents");
    let macos = contents.join("MacOS");
    let resources = contents.join("Resources");
    let result = (|| {
        check_dir(&macos)?;
        check_dir(&resources)?;
        write_new(&contents.join("Info.plist"), plist(a)?.as_bytes())?;
        let launcher = macos.join("launcher");
        write_new(&launcher, mac_launcher(a)?.as_bytes())?;
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(&launcher, std::fs::Permissions::from_mode(0o755))?;
        if let Some(icon) = &a.icon {
            write_new(&resources.join("icon.png"), &std::fs::read(icon)?)?;
        }
        for p in [&contents.join("Info.plist"), &launcher] {
            std::fs::File::open(p)?.sync_all()?;
        }
        for p in [&resources, &macos, &contents, &stage] {
            std::fs::File::open(p)?.sync_all()?;
        }
        Ok(())
    })();
    if let Err(e) = result {
        return Err(with_cleanup(e, super::transaction::remove_artifact(&stage)));
    }
    let entries = match std::fs::read_dir(root).and_then(|entries| {
        entries
            .map(|entry| entry.map(|entry| entry.path()))
            .collect::<io::Result<Vec<_>>>()
    }) {
        Ok(entries) => entries,
        Err(e) => return Err(with_cleanup(e, super::transaction::remove_artifact(&stage))),
    };
    let stale = entries
        .into_iter()
        .filter(|p| {
            p != &final_path
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.ends_with(&format!("~{}.app", a.hash)))
        })
        .collect::<Vec<_>>();
    super::transaction::publish(
        root,
        &final_path,
        &stage,
        &stale,
        |p| {
            if !crate::runtimes::store::is_plain_directory(p) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "unsafe owned bundle",
                ));
            }
            validate_bundle(p)
        },
        || Ok(()),
    )
}
#[cfg(windows)]
fn windows_install(root: &Path, filename: &str, a: &Artifact) -> io::Result<()> {
    use std::{
        process::{Command, Stdio},
        sync::atomic::{AtomicU64, Ordering},
        time::{Duration, Instant},
    };
    static NEXT: AtomicU64 = AtomicU64::new(0);
    const SCRIPT: &str = "$ErrorActionPreference='Stop'; $ws=New-Object -ComObject WScript.Shell; $s=$ws.CreateShortcut($env:ALEF_INSTALL_SHORTCUT); $s.TargetPath=$env:ALEF_INSTALL_EXE; $s.Arguments=$env:ALEF_INSTALL_ARGS; if ($env:ALEF_INSTALL_ICON) {$s.IconLocation=$env:ALEF_INSTALL_ICON}; $s.Save()";
    let dst = root.join(format!("{filename}.lnk"));
    let (temp, mut guard) = loop {
        let candidate = root.join(format!(
            ".alef-shortcut-stage-{}-{}.lnk",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&candidate)
        {
            Ok(mut file) => {
                use std::io::Write;
                let guard = StageGuard(Some(candidate.clone()));
                file.write_all(b"pending")?;
                file.sync_all()?;
                break (candidate, guard);
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(e) => return Err(e),
        }
    };
    let args = windows_arguments(&a.package)?;
    let mut command = Command::new("powershell.exe");
    command
        .args([
            "-NoProfile",
            "-NonInteractive",
            "-WindowStyle",
            "Hidden",
            "-Command",
            SCRIPT,
        ])
        .env("ALEF_INSTALL_EXE", &a.executable)
        .env("ALEF_INSTALL_ARGS", args)
        .env("ALEF_INSTALL_SHORTCUT", &temp)
        .env(
            "ALEF_INSTALL_ICON",
            a.icon.as_deref().unwrap_or(&a.executable),
        )
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .creation_flags(0x08000000);
    let mut child = command.spawn()?;
    let start = Instant::now();
    let status = loop {
        match child.try_wait() {
            Ok(Some(s)) => break Ok(s),
            Ok(None) if start.elapsed() < Duration::from_secs(10) => {
                std::thread::sleep(Duration::from_millis(20))
            }
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                break Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "shortcut creation timed out",
                ));
            }
            Err(e) => {
                let _ = child.kill();
                let _ = child.wait();
                break Err(e);
            }
        }
    };
    let result = match status {
        Ok(s) if s.success() => {
            if plain_file(&temp)? {
                std::fs::read(&temp)?;
                Ok(())
            } else {
                Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "shortcut creation failed",
                ))
            }
        }
        Ok(_) => Err(io::Error::other("shortcut creation failed")),
        Err(e) => Err(e),
    };
    if let Err(e) = result {
        let cleanup = super::transaction::remove_artifact(&temp);
        return Err(with_cleanup(e, cleanup));
    }
    let stale_result = (|| -> io::Result<Vec<PathBuf>> {
        let mut stale = Vec::new();
        let suffix = format!("~{}.lnk", a.hash);
        for entry in std::fs::read_dir(root)? {
            let p = entry?.path();
            if p != dst
                && p.file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.ends_with(&suffix))
            {
                stale.push(p);
            }
        }
        Ok(stale)
    })();
    let stale = match stale_result {
        Ok(v) => v,
        Err(e) => return Err(with_cleanup(e, super::transaction::remove_artifact(&temp))),
    };
    super::transaction::publish(
        root,
        &dst,
        &temp,
        &stale,
        |p| {
            if plain_file(p)? {
                Ok(())
            } else {
                Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "unsafe shortcut",
                ))
            }
        },
        || {
            #[cfg(all(windows, not(any(test, feature = "e2e"))))]
            {
                registry_write(a)?;
            }
            Ok(())
        },
    )?;
    guard.0 = None;
    Ok(())
}
#[cfg(windows)]
fn windows_remove(root: &Path, id: &str, _name: Option<&str>) -> io::Result<bool> {
    let suffix = format!("~{}.lnk", sha256_id(id));
    let mut removed = false;
    for e in std::fs::read_dir(root)? {
        let p = e?.path();
        if p.file_name()
            .and_then(|n| n.to_str())
            .is_some_and(|n| n.ends_with(&suffix))
        {
            removed |= remove_plain(&p)?;
        }
    }
    #[cfg(all(windows, not(any(test, feature = "e2e"))))]
    {
        removed |= registry_remove(id)?;
    }
    Ok(removed)
}
#[cfg(windows)]
use std::os::windows::process::CommandExt;
#[cfg(all(windows, not(any(test, feature = "e2e"))))]
fn registry_write(a: &Artifact) -> io::Result<()> {
    super::windows::registry_write(a)
}
#[cfg(all(windows, not(any(test, feature = "e2e"))))]
fn registry_remove(id: &str) -> io::Result<bool> {
    super::windows::registry_remove(id)
}
#[cfg(all(test, target_os = "macos"))]
#[path = "native_tests.rs"]
mod tests;
