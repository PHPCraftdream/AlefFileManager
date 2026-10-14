// SPDX-License-Identifier: MIT OR Apache-2.0
//! Safe filesystem primitives for the installed-application store.
use std::{
    io,
    path::{Component, Path, PathBuf},
    time::{Duration, Instant},
};

/// Stable-tree threat model: callers must prevent hostile renames during transactions.
pub fn create_checked_dir(path: &Path) -> io::Result<()> {
    let mut prefix = PathBuf::new();
    for component in path.components() {
        if matches!(component, Component::ParentDir) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "parent directory component refused",
            ));
        }
        prefix.push(component.as_os_str());
        if matches!(component, Component::Prefix(_))
            || matches!(component, Component::RootDir | Component::CurDir)
        {
            continue;
        }
        match std::fs::symlink_metadata(&prefix) {
            Ok(_) if crate::runtimes::store::is_plain_directory(&prefix) => {}
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "directory prefix is linked or not a directory",
                ));
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => match std::fs::create_dir(&prefix) {
                Ok(()) => set_private_permissions(&prefix)?,
                Err(e)
                    if e.kind() == io::ErrorKind::AlreadyExists
                        && crate::runtimes::store::is_plain_directory(&prefix) => {}
                Err(e) => return Err(e),
            },
            Err(e) => return Err(e),
        }
    }
    Ok(())
}

#[cfg(unix)]
fn set_private_permissions(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
}
#[cfg(not(unix))]
fn set_private_permissions(_: &Path) -> io::Result<()> {
    Ok(())
}

fn check_target(dir: &Path, destination: &Path) -> io::Result<()> {
    if !crate::runtimes::store::is_plain_directory(dir) || destination.parent() != Some(dir) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "invalid store directory or destination",
        ));
    }
    Ok(())
}

pub fn publish_bytes_no_replace(dir: &Path, destination: &Path, bytes: &[u8]) -> io::Result<()> {
    check_target(dir, destination)?;
    let (temp, mut file) = crate::runtimes::install::new_temp(dir)?;
    let result = (|| {
        use std::io::Write;
        file.write_all(bytes)?;
        file.flush()?;
        file.sync_all()?;
        drop(file);
        #[cfg(windows)]
        move_file(&temp, destination, 0)?;
        #[cfg(not(windows))]
        std::fs::hard_link(&temp, destination)?;
        sync_directory(dir);
        Ok(())
    })();
    let _ = std::fs::remove_file(temp);
    result
}

pub fn replace_record(dir: &Path, destination: &Path, bytes: &[u8]) -> io::Result<()> {
    check_target(dir, destination)?;
    match std::fs::symlink_metadata(destination) {
        Ok(meta) if plain_file_meta(&meta) => {}
        Ok(_) => {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "record destination is linked or not a file",
            ));
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(e),
    }
    let (temp, mut file) = crate::runtimes::install::new_temp(dir)?;
    let result = (|| {
        use std::io::Write;
        file.write_all(bytes)?;
        file.flush()?;
        file.sync_all()?;
        drop(file);
        #[cfg(windows)]
        move_file(&temp, destination, 1)?;
        #[cfg(not(windows))]
        std::fs::rename(&temp, destination)?;
        sync_directory(dir);
        Ok(())
    })();
    let _ = std::fs::remove_file(temp);
    result
}

fn plain_file_meta(meta: &std::fs::Metadata) -> bool {
    if meta.file_type().is_symlink() || !meta.is_file() {
        return false;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if meta.file_attributes() & 0x400 != 0 {
            return false;
        }
    }
    true
}
fn sync_directory(dir: &Path) {
    #[cfg(unix)]
    if let Ok(d) = std::fs::File::open(dir) {
        let _ = d.sync_all();
    }
    #[cfg(not(unix))]
    let _ = dir;
}

#[cfg(windows)]
fn move_file(from: &Path, to: &Path, flags: u32) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    let from: Vec<u16> = from.as_os_str().encode_wide().chain(Some(0)).collect();
    let to: Vec<u16> = to.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: both vectors are NUL-terminated UTF-16 paths and remain alive for the call.
    if unsafe {
        windows_sys::Win32::Storage::FileSystem::MoveFileExW(from.as_ptr(), to.as_ptr(), flags)
    } != 0
    {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}

#[derive(Debug)]
pub struct AppLock {
    _file: std::fs::File,
}
impl AppLock {
    /// Bounded lock wait; stable lock file is never removed.
    pub fn acquire(app_dir: &Path, timeout: Duration) -> io::Result<Self> {
        Self::acquire_named(app_dir, ".lock", timeout)
    }

    /// Acquires a stable named gate in an already-created directory.
    pub fn acquire_named(dir: &Path, name: &str, timeout: Duration) -> io::Result<Self> {
        if !crate::runtimes::store::is_plain_directory(dir)
            || Path::new(name).components().count() != 1
            || name == "."
            || name == ".."
        {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "invalid lock directory or name",
            ));
        }
        let path = dir.join(name);
        match std::fs::symlink_metadata(&path) {
            Ok(meta) if plain_file_meta(&meta) => {}
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "lock file is linked or not a regular file",
                ));
            }
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => return Err(e),
        }
        let file = std::fs::OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(&path)?;
        match std::fs::symlink_metadata(&path) {
            Ok(meta) if plain_file_meta(&meta) => {}
            Ok(_) => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "lock file is linked or not a regular file",
                ));
            }
            Err(e) => return Err(e),
        }
        let start = Instant::now();
        loop {
            match file.try_lock() {
                Ok(()) => return Ok(Self { _file: file }),
                Err(std::fs::TryLockError::WouldBlock) if start.elapsed() < timeout => {
                    std::thread::sleep(
                        Duration::from_millis(10).min(timeout.saturating_sub(start.elapsed())),
                    )
                }
                Err(std::fs::TryLockError::WouldBlock) => {
                    return Err(io::Error::new(io::ErrorKind::TimedOut, "BUSY"));
                }
                Err(std::fs::TryLockError::Error(error)) => return Err(error),
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Scratch(PathBuf);
    impl std::ops::Deref for Scratch {
        type Target = Path;
        fn deref(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn scratch() -> Scratch {
        loop {
            let path = std::env::temp_dir().join(format!(
                "alef-install-store-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match std::fs::create_dir(&path) {
                Ok(()) => return Scratch(std::fs::canonicalize(&path).expect("canonical scratch")),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => panic!("create scratch directory: {e}"),
            }
        }
    }
    #[test]
    fn publishing_bytes_without_replacement_and_replacing_records_works() {
        let dir = scratch();
        let dst = dir.join("record");
        publish_bytes_no_replace(&dir, &dst, b"one").unwrap();
        assert!(publish_bytes_no_replace(&dir, &dst, b"two").is_err());
        replace_record(&dir, &dst, b"three").unwrap();
        assert_eq!(std::fs::read(&dst).unwrap(), b"three");
        // Scratch removes the directory on drop.
    }
    #[test]
    fn creating_directories_concurrently_and_locking_with_timeout_works() {
        let base = scratch();
        let dir = base.join("a/b");
        let a = dir.clone();
        let b = dir.clone();
        let t1 = std::thread::spawn(move || create_checked_dir(&a).unwrap());
        let t2 = std::thread::spawn(move || create_checked_dir(&b).unwrap());
        t1.join().unwrap();
        t2.join().unwrap();
        let held = AppLock::acquire(&dir, Duration::ZERO).unwrap();
        assert_eq!(
            AppLock::acquire(&dir, Duration::from_millis(20))
                .unwrap_err()
                .kind(),
            io::ErrorKind::TimedOut
        );
        drop(held);
        AppLock::acquire(&dir, Duration::ZERO).unwrap();
        // Scratch removes the directory on drop.
    }
    #[cfg(unix)]
    #[test]
    fn acquiring_a_linked_lock_is_rejected() {
        use std::os::unix::fs::symlink;
        let dir = scratch();
        let target = dir.join("target");
        std::fs::write(&target, b"").unwrap();
        symlink(&target, dir.join(".lock")).unwrap();
        assert_eq!(
            AppLock::acquire(&dir, Duration::ZERO).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        // Scratch removes the directory on drop.
    }
}
