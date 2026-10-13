// SPDX-License-Identifier: MIT OR Apache-2.0
//! Publishing one runtime version into the store: copy through a same-folder temp file
//! and publish atomically, never clobbering an already installed version.
//! The store tree must not be replaced by another process during a transaction;
//! path-based metadata checks reject existing links, not hostile rename races.
use std::{
    io::{self, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use crate::runtimes::store::{
    self, installed_in, is_plain_semver, is_regular_file, StoreError, BINARY_NAME,
};

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// `ERROR_ALREADY_EXISTS`: a no-clobber publish loses to an existing file.
#[cfg(windows)]
const ERROR_ALREADY_EXISTS: i32 = 183;

/// Installs `version` from `source_binary` into `root`, returning the published binary path.
pub fn install_in(root: &Path, version: &str, source_binary: &Path) -> Result<PathBuf, StoreError> {
    if !is_plain_semver(version) {
        return Err(StoreError::InvalidArgument(format!(
            "{version:?} is not a strict MAJOR.MINOR.PATCH version"
        )));
    }
    let source_metadata = match std::fs::symlink_metadata(source_binary) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            return Err(StoreError::InvalidArgument(format!(
                "{}: the source binary does not exist",
                source_binary.display()
            )));
        }
        Err(error) => return Err(StoreError::Store(error)),
    };
    if !is_regular_file(source_binary) || source_metadata.is_dir() {
        return Err(StoreError::InvalidArgument(format!(
            "{}: the source binary is not a regular file",
            source_binary.display()
        )));
    }
    if installed_in(root)
        .map_err(StoreError::Store)?
        .iter()
        .any(|entry| entry.version == version)
    {
        return Err(StoreError::AlreadyInstalled(format!(
            "{version} is already installed"
        )));
    }
    let root = root.components().collect::<PathBuf>();
    let root = root.as_path();
    store::check_root(root).map_err(StoreError::Store)?;
    std::fs::create_dir_all(root).map_err(StoreError::Store)?;
    store::check_root(root).map_err(StoreError::Store)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(root, std::fs::Permissions::from_mode(0o700))
            .map_err(StoreError::Store)?;
    }
    create_and_publish(root, version, source_binary)
}

/// Only the successful creator owns rollback of the empty version folder.
fn create_and_publish(
    root: &Path,
    version: &str,
    source_binary: &Path,
) -> Result<PathBuf, StoreError> {
    let folder = root.join(version);
    if let Err(error) = std::fs::create_dir(&folder) {
        if error.kind() == io::ErrorKind::AlreadyExists && store::is_plain_directory(&folder) {
            return Err(StoreError::AlreadyInstalled(format!(
                "{version} is already installed"
            )));
        }
        return Err(StoreError::Store(error));
    }
    let result = (|| {
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            std::fs::set_permissions(&folder, std::fs::Permissions::from_mode(0o700))
                .map_err(StoreError::Store)?;
        }
        publish(root, &folder, version, source_binary)
    })();
    if result.is_err() {
        let _ = std::fs::remove_dir(&folder);
    }
    result
}

/// Copies the source through a temp file in `folder`'s parent and publishes no-clobber.
fn publish(
    root: &Path,
    folder: &Path,
    version: &str,
    source_binary: &Path,
) -> Result<PathBuf, StoreError> {
    store::check_root(root).map_err(StoreError::Store)?;
    if !store::is_plain_directory(folder) {
        return Err(StoreError::Store(io::Error::new(
            io::ErrorKind::InvalidData,
            "the version folder is not a plain directory",
        )));
    }
    let (temp, mut file) = new_temp(root).map_err(StoreError::Store)?;
    let cleanup = |result: Result<PathBuf, StoreError>| -> Result<PathBuf, StoreError> {
        let _ = std::fs::remove_file(&temp);
        result
    };
    let copied = || -> io::Result<()> {
        let mut source = std::fs::File::open(source_binary)?;
        io::copy(&mut source, &mut file)?;
        file.flush()?;
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            file.set_permissions(std::fs::Permissions::from_mode(0o755))?;
        }
        file.sync_all()?;
        Ok(())
    }();
    drop(file);
    if let Err(error) = copied {
        return cleanup(Err(StoreError::Store(error)));
    }
    let final_path = folder.join(BINARY_NAME);
    #[cfg(unix)]
    if let Err(error) = std::fs::hard_link(&temp, &final_path) {
        return cleanup(cleanup_result(version, error));
    }
    #[cfg(windows)]
    {
        let temp_wide = wide(&temp);
        let final_wide = wide(&final_path);
        // SAFETY: both vectors hold NUL-terminated UTF-16 paths and remain alive
        // throughout this synchronous call; flags 0 never replace a destination.
        if unsafe {
            windows_sys::Win32::Storage::FileSystem::MoveFileExW(
                temp_wide.as_ptr(),
                final_wide.as_ptr(),
                0,
            )
        } == 0
        {
            let error = io::Error::last_os_error();
            return cleanup(cleanup_result(version, error));
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let error = io::Error::other("runtime installation is not supported on this platform");
        return cleanup(Err(StoreError::Store(error)));
    }
    settle(folder, &temp);
    Ok(final_path)
}

// After a successful publish: drop the temp link and sync the folder, best effort.
fn settle(folder: &Path, temp: &Path) {
    let _ = std::fs::remove_file(temp);
    #[cfg(unix)]
    if let Ok(dir) = std::fs::File::open(folder) {
        let _ = dir.sync_all();
    }
}

/// The loser of the publication race reports the winner's version as already installed.
fn cleanup_result(version: &str, error: io::Error) -> Result<PathBuf, StoreError> {
    let already_exists = error.kind() == io::ErrorKind::AlreadyExists;
    #[cfg(windows)]
    let already_exists = already_exists || error.raw_os_error() == Some(ERROR_ALREADY_EXISTS);
    if already_exists {
        return Err(StoreError::AlreadyInstalled(format!(
            "{version} is already installed"
        )));
    }
    Err(StoreError::Store(error))
}

/// Exclusive creation owns cleanup; collisions are never overwritten or deleted.
fn new_temp(root: &Path) -> io::Result<(PathBuf, std::fs::File)> {
    loop {
        // Relaxed allocates a unique candidate, not a publication signal.
        let unique = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
        let temp = root.join(format!(".tmp-{}-{unique}", std::process::id()));
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&temp)
        {
            Ok(file) => return Ok((temp, file)),
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
            Err(error) => return Err(error),
        }
    }
}

/// Wide (UTF-16, NUL-terminated) path for the Windows API.
#[cfg(windows)]
fn wide(path: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// [`install_in`] with the launcher's default store root.
pub fn install(version: &str, source_binary: &Path) -> Result<PathBuf, StoreError> {
    install_in(&store::root(), version, source_binary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtimes::support::Scratch;

    fn fake_binary(scratch: &Path, name: &str, byte: u8) -> PathBuf {
        let path = scratch.join(name);
        std::fs::write(&path, [byte; 8]).unwrap();
        path
    }

    fn place(root: &Path, version: &str, content: &[u8]) {
        let folder = root.join(version);
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join(BINARY_NAME), content).unwrap();
    }

    fn no_temp_files(root: &Path) -> bool {
        !std::fs::read_dir(root).unwrap().any(|entry| {
            entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with(".tmp-")
        })
    }

    #[test]
    fn install_publishes_a_regular_executable_into_the_version_folder() {
        let temp = Scratch::new();
        let scratch = Scratch::new();
        let root = temp.path();
        let source = fake_binary(scratch.path(), "alef.bin", 7);
        let final_path = install_in(root, "1.2.3", &source).unwrap();
        assert_eq!(final_path, root.join("1.2.3").join(BINARY_NAME));
        assert!(is_regular_file(&final_path));
        assert_eq!(std::fs::read(&final_path).unwrap(), vec![7; 8]);
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(&final_path).unwrap().permissions().mode();
            assert_eq!(mode & 0o777, 0o755);
            let root_mode = std::fs::metadata(root).unwrap().permissions().mode();
            assert_eq!(root_mode & 0o777, 0o700);
            let folder_mode = std::fs::metadata(root.join("1.2.3"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(folder_mode & 0o777, 0o700);
        }
        assert!(no_temp_files(root));
    }

    #[test]
    fn install_refuses_an_already_installed_version_and_leaves_it_untouched() {
        let temp = Scratch::new();
        let scratch = Scratch::new();
        let root = temp.path();
        place(root, "1.0.0", b"AAAA");
        let source = fake_binary(scratch.path(), "alef.bin", 9);
        let error = install_in(root, "1.0.0", &source).unwrap_err();
        assert!(matches!(error, StoreError::AlreadyInstalled(_)));
        assert_eq!(error.exit(), 2);
        assert_eq!(
            std::fs::read(root.join("1.0.0").join(BINARY_NAME)).unwrap(),
            b"AAAA"
        );
        assert!(no_temp_files(root));
    }

    #[test]
    fn install_refuses_a_missing_source_and_a_directory_as_the_source() {
        let temp = Scratch::new();
        let scratch = Scratch::new();
        let root = temp.path();
        let missing = install_in(root, "1.0.0", &scratch.path().join("nope")).unwrap_err();
        assert!(matches!(missing, StoreError::InvalidArgument(_)));
        assert_eq!(missing.exit(), 2);
        let directory = install_in(root, "1.0.0", scratch.path()).unwrap_err();
        assert!(matches!(directory, StoreError::InvalidArgument(_)));
        assert_eq!(directory.exit(), 2);
        assert!(installed_in(root).unwrap().is_empty());
    }

    #[test]
    fn install_refuses_a_version_that_is_not_strict_semver() {
        let temp = Scratch::new();
        let scratch = Scratch::new();
        let source = fake_binary(scratch.path(), "alef.bin", 1);
        for version in ["1.0", "1.0.0-beta", "garbage"] {
            let error = install_in(temp.path(), version, &source).unwrap_err();
            assert!(matches!(error, StoreError::InvalidArgument(_)));
            assert_eq!(error.exit(), 2);
        }
    }

    #[test]
    fn a_failed_install_removes_the_version_folder_it_created() {
        let temp = Scratch::new();
        let scratch = Scratch::new();
        let root = temp.path();
        let error = create_and_publish(root, "1.0.0", &scratch.path().join("missing")).unwrap_err();
        assert!(matches!(error, StoreError::Store(_)));
        assert!(!root.join("1.0.0").exists());
        assert!(no_temp_files(root));
    }

    #[test]
    fn install_leaves_no_temporary_file_after_a_failed_publication() {
        let temp = Scratch::new();
        let scratch = Scratch::new();
        let root = temp.path();
        place(root, "1.0.0", b"AAAA");
        let source = fake_binary(scratch.path(), "alef.bin", 4);
        let error = publish(root, &root.join("1.0.0"), "1.0.0", &source).unwrap_err();
        assert!(matches!(error, StoreError::AlreadyInstalled(_)));
        assert_eq!(
            std::fs::read(root.join("1.0.0").join(BINARY_NAME)).unwrap(),
            b"AAAA"
        );
        assert!(no_temp_files(root));
    }

    #[test]
    fn two_installs_of_the_same_version_race_and_exactly_one_wins() {
        let temp = Scratch::new();
        let scratch = Scratch::new();
        let root = temp.path();
        let source_a = fake_binary(scratch.path(), "a.bin", 1);
        let source_b = fake_binary(scratch.path(), "b.bin", 2);
        let (sender, receiver) = std::sync::mpsc::sync_channel(2);
        let mut starts = Vec::new();
        let handles: Vec<_> = [source_a, source_b]
            .into_iter()
            .map(|source| {
                let sender = sender.clone();
                let root = root.to_path_buf();
                let (start, started) = std::sync::mpsc::sync_channel(1);
                starts.push(start);
                std::thread::spawn(move || {
                    started
                        .recv_timeout(std::time::Duration::from_secs(60))
                        .unwrap();
                    sender.send(install_in(&root, "2.0.0", &source)).unwrap();
                })
            })
            .collect();
        for start in starts {
            start.send(()).unwrap();
        }
        drop(sender);
        let timeout = std::time::Duration::from_secs(60);
        let first = receiver.recv_timeout(timeout).unwrap();
        let second = receiver.recv_timeout(timeout).unwrap();
        for handle in handles {
            handle.join().unwrap();
        }
        let results = [first, second];
        assert_eq!(results.iter().filter(|r| r.is_ok()).count(), 1);
        assert!(results.iter().all(|r| match r {
            Ok(_) => true,
            Err(error) => matches!(error, StoreError::AlreadyInstalled(_)),
        }));
        let bytes = std::fs::read(root.join("2.0.0").join(BINARY_NAME)).unwrap();
        assert!(bytes == vec![1; 8] || bytes == vec![2; 8]);
        assert!(is_regular_file(&root.join("2.0.0").join(BINARY_NAME)));
        assert!(no_temp_files(root));
        assert_eq!(installed_in(root).unwrap().len(), 1);
    }

    #[test]
    fn install_does_not_clean_up_an_existing_folder_or_unknown_file() {
        let temp = Scratch::new();
        let scratch = Scratch::new();
        let source = fake_binary(scratch.path(), "source", 8);
        std::fs::create_dir(temp.path().join("1.0.0")).unwrap();
        assert!(matches!(
            install_in(temp.path(), "1.0.0", &source),
            Err(StoreError::AlreadyInstalled(_))
        ));
        assert!(temp.path().join("1.0.0").is_dir());
        std::fs::write(temp.path().join("2.0.0"), b"sentinel").unwrap();
        assert!(matches!(
            install_in(temp.path(), "2.0.0", &source),
            Err(StoreError::Store(_))
        ));
        assert_eq!(
            std::fs::read(temp.path().join("2.0.0")).unwrap(),
            b"sentinel"
        );
        assert!(no_temp_files(temp.path()));
    }

    #[cfg(unix)]
    #[test]
    fn install_refuses_links_for_the_root_version_folder_and_source() {
        let temp = Scratch::new();
        let outside = Scratch::new();
        let source = fake_binary(outside.path(), "source", 5);
        let root_link = temp.path().join("root-link");
        std::os::unix::fs::symlink(outside.path(), &root_link).unwrap();
        assert!(matches!(
            install_in(&root_link, "1.0.0", &source),
            Err(StoreError::Store(_))
        ));
        std::os::unix::fs::symlink(outside.path(), temp.path().join("1.0.0")).unwrap();
        assert!(matches!(
            install_in(temp.path(), "1.0.0", &source),
            Err(StoreError::Store(_))
        ));
        let source_link = temp.path().join("source-link");
        std::os::unix::fs::symlink(&source, &source_link).unwrap();
        let error = install_in(temp.path(), "2.0.0", &source_link).unwrap_err();
        assert!(matches!(error, StoreError::InvalidArgument(_)));
        assert_eq!(error.exit(), 2);
        assert!(!outside.path().join("1.0.0").exists());
        assert_eq!(std::fs::read(source).unwrap(), vec![5; 8]);
        assert!(std::fs::symlink_metadata(temp.path().join("1.0.0"))
            .unwrap()
            .file_type()
            .is_symlink());
    }
}
