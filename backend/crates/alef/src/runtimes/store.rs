// SPDX-License-Identifier: MIT OR Apache-2.0
//! The layout of the runtime store: one strict-semver folder per installed version,
//! each holding exactly one launcher binary, listed and selected fail-closed.
use std::{
    fmt, io,
    path::{Path, PathBuf},
};

use alef_core::security::manifest::{runtime_requirement_met, validate_package_version};

use crate::consent::runtime_home;

/// `FILE_ATTRIBUTE_REPARSE_POINT`: any link-like Windows object is refused.
#[cfg(windows)]
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

/// The binary file name every installed version folder must contain.
#[cfg(windows)]
pub const BINARY_NAME: &str = "alef.exe";
/// The binary file name every installed version folder must contain.
#[cfg(not(windows))]
pub const BINARY_NAME: &str = "alef";

/// The root folder of the runtime store under the launcher's runtime home.
pub fn root() -> PathBuf {
    runtime_home().join("runtimes")
}

/// One installed runtime version and the path of its binary.
/// Versions stay strings: this crate has no direct dependency on the parser's type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Installed {
    /// The exact installed version, a strict `MAJOR.MINOR.PATCH` string.
    pub version: String,
    /// The published binary inside the version folder.
    pub binary: PathBuf,
}

/// Why a store command did not complete.
#[derive(Debug)]
pub enum StoreError {
    /// Exit 2: the inputs themselves are unusable.
    InvalidArgument(String),
    /// Exit 2: the version is already in the store.
    AlreadyInstalled(String),
    /// Exit 1: the store failed.
    Store(io::Error),
}

impl StoreError {
    pub fn exit(&self) -> u8 {
        match self {
            Self::InvalidArgument(_) | Self::AlreadyInstalled(_) => 2,
            Self::Store(_) => 1,
        }
    }
}

impl fmt::Display for StoreError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidArgument(message) => write!(f, "INVALID_ARGUMENT: {message}"),
            Self::AlreadyInstalled(message) => write!(f, "ALREADY_INSTALLED: {message}"),
            Self::Store(error) => write!(f, "RUNTIME_STORE: {error}"),
        }
    }
}

/// True only for a strict `MAJOR.MINOR.PATCH` version: no prerelease, no build metadata.
pub(crate) fn is_plain_semver(name: &str) -> bool {
    let version = match validate_package_version(name) {
        Ok(version) => version,
        Err(_) => return false,
    };
    if !version.pre.is_empty() || !version.build.is_empty() {
        return false;
    }
    true
}

/// True only for a plain regular file: no symlink, no Windows reparse attribute.
pub(crate) fn is_regular_file(path: &Path) -> bool {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(_) => return false,
    };
    if metadata.file_type().is_symlink() {
        return false;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return false;
        }
    }
    metadata.is_file()
}

/// True only for a directory itself, not a symlink or reparse point.
pub(crate) fn is_plain_directory(path: &Path) -> bool {
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(_) => return false,
    };
    if metadata.file_type().is_symlink() {
        return false;
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt as _;
        if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
            return false;
        }
    }
    metadata.is_dir()
}

/// Rejects a non-directory root before any traversal; missing roots are allowed.
pub(crate) fn check_root(root: &Path) -> io::Result<()> {
    match std::fs::symlink_metadata(root) {
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(error) => Err(error),
        Ok(_) if is_plain_directory(root) => Ok(()),
        Ok(_) => Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the runtime store root is not a plain directory",
        )),
    }
}

/// Lists every installed version in `root`, oldest first.
pub fn installed_in(root: &Path) -> io::Result<Vec<Installed>> {
    let root = root.components().collect::<PathBuf>();
    check_root(&root)?;
    let entries = match std::fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
        Err(error) => return Err(error),
    };
    let mut installed = Vec::new();
    for entry in entries {
        let entry = entry?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !is_plain_semver(&name) || !is_plain_directory(&entry.path()) {
            continue;
        }
        if is_regular_file(&entry.path().join(BINARY_NAME)) {
            installed.push(Installed {
                version: name,
                binary: entry.path().join(BINARY_NAME),
            });
        }
    }
    installed.sort_by(|a, b| {
        let a = validate_package_version(&a.version).expect("plain semver checked");
        let b = validate_package_version(&b.version).expect("plain semver checked");
        a.cmp(&b)
    });
    Ok(installed)
}

/// Lists every installed version, oldest first.
pub fn installed() -> io::Result<Vec<Installed>> {
    installed_in(&root())
}

/// The highest installed version in `root` that satisfies `requirement`, if any.
pub fn select_in(root: &Path, requirement: &str) -> io::Result<Option<Installed>> {
    Ok(installed_in(root)?
        .into_iter()
        .rev()
        .find(|entry| runtime_requirement_met(requirement, &entry.version)))
}

/// The highest installed version that satisfies `requirement`, if any.
pub fn select(requirement: &str) -> io::Result<Option<Installed>> {
    select_in(&root(), requirement)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtimes::support::Scratch;

    fn place(root: &Path, version: &str) -> PathBuf {
        let folder = root.join(version);
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join(BINARY_NAME), version.as_bytes()).unwrap();
        folder
    }

    #[test]
    fn installed_ignores_folders_that_are_not_strict_semver_names() {
        let temp = Scratch::new();
        for name in [
            "1.0",
            "1.0.0-beta",
            "1.0.0+build",
            "01.2.3",
            "v1.0.0",
            "notaversion",
        ] {
            place(temp.path(), name);
        }
        assert!(installed_in(temp.path()).unwrap().is_empty());
        assert!(is_plain_semver("1.0.0"));
        assert!(!is_plain_semver("1.0.0-beta+build"));
    }

    #[test]
    fn installed_ignores_a_plain_file_in_place_of_a_version_folder() {
        let temp = Scratch::new();
        std::fs::write(temp.path().join("1.0.0"), "a file").unwrap();
        assert!(installed_in(temp.path()).unwrap().is_empty());
    }

    #[test]
    fn installed_ignores_a_version_folder_without_a_regular_binary() {
        let temp = Scratch::new();
        std::fs::create_dir_all(temp.path().join("1.0.0")).unwrap();
        std::fs::create_dir_all(temp.path().join("2.0.0").join(BINARY_NAME)).unwrap();
        #[cfg(unix)]
        {
            let real = temp.path().join("real");
            std::fs::write(&real, "content").unwrap();
            std::fs::create_dir_all(temp.path().join("3.0.0")).unwrap();
            std::os::unix::fs::symlink(&real, temp.path().join("3.0.0").join(BINARY_NAME)).unwrap();
        }
        assert!(installed_in(temp.path()).unwrap().is_empty());
    }

    #[test]
    fn installed_lists_versions_from_oldest_to_newest() {
        let temp = Scratch::new();
        let root = temp.path();
        place(root, "0.1.0");
        place(root, "0.10.0");
        place(root, "0.2.0");
        place(root, "1.0.0");
        let installed = installed_in(root).unwrap();
        let versions: Vec<&str> = installed.iter().map(|e| e.version.as_str()).collect();
        assert_eq!(versions, ["0.1.0", "0.2.0", "0.10.0", "1.0.0"]);
        for entry in &installed {
            assert_eq!(entry.binary, root.join(&entry.version).join(BINARY_NAME));
        }
    }

    #[test]
    fn a_missing_store_root_counts_as_no_runtimes_installed() {
        let temp = Scratch::new();
        assert_eq!(
            installed_in(&temp.path().join("missing")).unwrap(),
            Vec::new()
        );
    }

    #[test]
    fn select_picks_the_highest_installed_version_that_satisfies_the_requirement() {
        let temp = Scratch::new();
        let root = temp.path();
        place(root, "1.0.0");
        place(root, "1.2.0");
        place(root, "2.0.0");
        assert_eq!(
            select_in(root, "^1").unwrap().map(|e| e.version),
            Some("1.2.0".to_owned())
        );
        assert_eq!(
            select_in(root, "=1.0.0").unwrap().map(|e| e.version),
            Some("1.0.0".to_owned())
        );
        place(root, "0.2.0");
        assert_eq!(select_in(root, "^0.1").unwrap(), None);
    }

    #[test]
    fn select_returns_none_when_no_installed_version_fits() {
        let temp = Scratch::new();
        let root = temp.path();
        place(root, "0.1.0");
        assert_eq!(select_in(root, "^999").unwrap(), None);
        assert_eq!(
            select_in(&temp.path().join("missing"), "^0.1").unwrap(),
            None
        );
    }

    #[cfg(unix)]
    #[test]
    fn listing_refuses_a_link_root_and_ignores_a_link_version_folder() {
        let temp = Scratch::new();
        let outside = Scratch::new();
        place(outside.path(), "1.0.0");
        let linked_root = temp.path().join("linked-root");
        std::os::unix::fs::symlink(outside.path(), &linked_root).unwrap();
        assert!(installed_in(&linked_root).is_err());
        std::os::unix::fs::symlink(outside.path().join("1.0.0"), temp.path().join("2.0.0"))
            .unwrap();
        assert!(installed_in(temp.path()).unwrap().is_empty());
        assert_eq!(
            std::fs::read(outside.path().join("1.0.0").join(BINARY_NAME)).unwrap(),
            b"1.0.0"
        );
    }
}
