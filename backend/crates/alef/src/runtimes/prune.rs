// SPDX-License-Identifier: MIT OR Apache-2.0
//! Removing installed runtime versions that are no longer kept: only proven link-free
//! version folders are ever deleted, and the running version is always kept.
//! The tree must remain stable between the path-based walk and deletion;
//! this is not confinement against a process concurrently replacing directories.
use std::{collections::HashSet, path::Path};

use alef_core::security::manifest::validate_package_version;

use crate::runtimes::store::{
    self, is_plain_directory, is_plain_semver, is_regular_file, StoreError, BINARY_NAME,
};

/// The folder depth limit of the safety walk below which removal is allowed.
const WALK_DEPTH_LIMIT: usize = 64;
/// `FILE_ATTRIBUTE_REPARSE_POINT`: any link-like Windows object is refused.
#[cfg(windows)]
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;

/// What a prune run did or would have done.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct PruneReport {
    /// Versions actually removed (empty in a dry run).
    pub removed: Vec<String>,
    /// Versions a real run would remove (a dry run removes nothing).
    pub would_remove: Vec<String>,
    /// Versions kept on disk because the safety walk found a link or worse.
    pub skipped: Vec<(String, String)>,
}

/// Removes every installed version in `root` that is neither in `keep` nor running,
/// unless `dry_run` only reports them.
pub fn prune_in(root: &Path, keep: &[String], dry_run: bool) -> Result<PruneReport, StoreError> {
    let mut keep_set: HashSet<&str> = HashSet::new();
    for version in keep {
        if !is_plain_semver(version) {
            return Err(StoreError::InvalidArgument(format!(
                "{version:?} is not a strict MAJOR.MINOR.PATCH version"
            )));
        }
        keep_set.insert(version.as_str());
    }
    keep_set.insert(env!("CARGO_PKG_VERSION"));
    let mut report = PruneReport::default();
    let root = root.components().collect::<std::path::PathBuf>();
    store::check_root(&root).map_err(StoreError::Store)?;
    let entries = match std::fs::read_dir(&root) {
        Ok(entries) => entries,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => return Ok(report),
        Err(error) => return Err(StoreError::Store(error)),
    };
    for entry in entries {
        let entry = entry.map_err(StoreError::Store)?;
        let name = entry.file_name().to_string_lossy().into_owned();
        if !is_plain_semver(&name) || keep_set.contains(name.as_str()) {
            continue;
        }
        // Check the folder itself before looking at any child.
        if !is_plain_directory(&entry.path()) {
            continue;
        }
        if !is_regular_file(&entry.path().join(BINARY_NAME)) {
            continue;
        }
        match walk(&entry.path(), 0) {
            Ok(true) => {}
            Ok(false) => {
                report.skipped.push((
                    name.clone(),
                    "the version folder contains a link or an unknown entry".to_owned(),
                ));
                continue;
            }
            Err(error) => return Err(StoreError::Store(error)),
        }
        if dry_run {
            report.would_remove.push(name);
        } else {
            std::fs::remove_dir_all(entry.path()).map_err(StoreError::Store)?;
            report.removed.push(name);
        }
    }
    sort_versions(&mut report.removed);
    sort_versions(&mut report.would_remove);
    report.skipped.sort_by(|a, b| compare(&a.0, &b.0));
    Ok(report)
}

/// Sorts plain semver names by parsed version; names are already validated.
fn sort_versions(names: &mut [String]) {
    names.sort_by(|a, b| compare(a, b));
}

/// Compares two plain semver names by parsed version.
fn compare(a: &str, b: &str) -> std::cmp::Ordering {
    let a = validate_package_version(a).expect("plain semver checked");
    let b = validate_package_version(b).expect("plain semver checked");
    a.cmp(&b)
}

/// True when every entry under `folder` is a real directory or a real file, no links,
/// with a depth cap so a pathological tree cannot recurse forever.
fn walk(folder: &Path, depth: usize) -> std::io::Result<bool> {
    if depth > WALK_DEPTH_LIMIT || !is_plain_directory(folder) {
        return Ok(false);
    }
    for entry in std::fs::read_dir(folder)? {
        let entry = entry?;
        let metadata = std::fs::symlink_metadata(entry.path())?;
        if metadata.file_type().is_symlink() {
            return Ok(false);
        }
        #[cfg(windows)]
        {
            use std::os::windows::fs::MetadataExt as _;
            if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
                return Ok(false);
            }
        }
        if metadata.is_dir() {
            if !walk(&entry.path(), depth + 1)? {
                return Ok(false);
            }
        } else if !metadata.is_file() {
            return Ok(false);
        }
    }
    Ok(true)
}

/// [`prune_in`] with the launcher's default store root.
pub fn prune(keep: &[String], dry_run: bool) -> Result<PruneReport, StoreError> {
    prune_in(&crate::runtimes::store::root(), keep, dry_run)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::runtimes::support::Scratch;

    fn place(root: &Path, version: &str) {
        let folder = root.join(version);
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join(BINARY_NAME), version.as_bytes()).unwrap();
    }

    fn exists(root: &Path, version: &str) -> bool {
        root.join(version).join(BINARY_NAME).is_file()
    }

    #[test]
    fn prune_removes_installed_versions_not_kept_and_reports_them() {
        let temp = Scratch::new();
        let root = temp.path();
        place(root, "1.0.0");
        place(root, "1.1.0");
        place(root, "2.0.0");
        let report = prune_in(root, &["1.0.0".to_owned()], false).unwrap();
        assert_eq!(report.removed, ["1.1.0", "2.0.0"]);
        assert!(report.would_remove.is_empty());
        assert!(report.skipped.is_empty());
        assert!(exists(root, "1.0.0"));
        assert!(!root.join("1.1.0").exists());
        assert!(!root.join("2.0.0").exists());
    }

    #[test]
    fn prune_never_removes_the_running_version() {
        let temp = Scratch::new();
        let root = temp.path();
        let running = env!("CARGO_PKG_VERSION");
        place(root, running);
        let report = prune_in(root, &[], false).unwrap();
        assert!(report.removed.is_empty());
        assert!(exists(root, running));
    }

    #[test]
    fn prune_does_not_touch_unknown_entries() {
        let temp = Scratch::new();
        let root = temp.path();
        std::fs::create_dir_all(root.join("hello")).unwrap();
        std::fs::write(root.join("1.5.0"), "a file").unwrap();
        std::fs::create_dir_all(root.join("3.0.0")).unwrap();
        let report = prune_in(root, &[], false).unwrap();
        assert!(report.removed.is_empty());
        assert!(root.join("hello").is_dir());
        assert!(root.join("1.5.0").is_file());
        assert!(root.join("3.0.0").is_dir());
    }

    #[cfg(unix)]
    #[test]
    fn prune_does_not_remove_a_version_folder_that_contains_a_link() {
        let temp = Scratch::new();
        let root = temp.path();
        place(root, "2.0.0");
        let target = root.join("target");
        std::fs::write(&target, "content").unwrap();
        std::os::unix::fs::symlink(&target, root.join("2.0.0").join("link")).unwrap();
        let report = prune_in(root, &[], false).unwrap();
        assert_eq!(
            report.skipped,
            vec![(
                "2.0.0".to_owned(),
                "the version folder contains a link or an unknown entry".to_owned()
            )]
        );
        assert!(exists(root, "2.0.0"));
    }

    #[test]
    fn a_dry_run_removes_nothing_and_reports_the_same_set_as_a_real_run() {
        let temp = Scratch::new();
        let root = temp.path();
        place(root, "1.0.0");
        place(root, "1.1.0");
        let dry = prune_in(root, &["1.0.0".to_owned()], true).unwrap();
        assert_eq!(dry.would_remove, ["1.1.0"]);
        assert!(dry.removed.is_empty());
        assert!(exists(root, "1.1.0"));
        let real = prune_in(root, &["1.0.0".to_owned()], false).unwrap();
        assert_eq!(real.removed, dry.would_remove);
        assert!(!exists(root, "1.1.0"));
    }

    #[test]
    fn prune_refuses_a_keep_entry_that_is_not_strict_semver() {
        let temp = Scratch::new();
        let root = temp.path();
        place(root, "1.0.0");
        let error = prune_in(root, &["1.0".to_owned()], false).unwrap_err();
        assert!(matches!(error, StoreError::InvalidArgument(_)));
        assert_eq!(error.exit(), 2);
        assert!(exists(root, "1.0.0"));
    }

    #[cfg(unix)]
    #[test]
    fn prune_refuses_a_link_root_and_never_follows_version_or_nested_links() {
        let temp = Scratch::new();
        let outside = Scratch::new();
        place(outside.path(), "2.0.0");
        let sentinel = outside.path().join("sentinel");
        std::fs::write(&sentinel, b"untouched").unwrap();
        let root_link = temp.path().join("root-link");
        std::os::unix::fs::symlink(outside.path(), &root_link).unwrap();
        assert!(matches!(
            prune_in(&root_link, &[], false),
            Err(StoreError::Store(_))
        ));
        std::os::unix::fs::symlink(outside.path().join("2.0.0"), temp.path().join("2.0.0"))
            .unwrap();
        place(temp.path(), "3.0.0");
        let nested = temp.path().join("3.0.0").join("nested");
        std::fs::create_dir(&nested).unwrap();
        std::os::unix::fs::symlink(outside.path(), nested.join("link")).unwrap();
        let report = prune_in(temp.path(), &[], false).unwrap();
        assert!(report.removed.is_empty());
        assert_eq!(report.skipped.len(), 1);
        assert_eq!(report.skipped[0].0, "3.0.0");
        assert!(exists(outside.path(), "2.0.0"));
        assert!(exists(temp.path(), "3.0.0"));
        assert!(std::fs::symlink_metadata(temp.path().join("2.0.0"))
            .unwrap()
            .file_type()
            .is_symlink());
        assert_eq!(std::fs::read(sentinel).unwrap(), b"untouched");
    }

    #[test]
    fn prune_removes_nested_regular_directories_and_sorts_by_numeric_version() {
        let temp = Scratch::new();
        for version in ["4.10.0", "4.2.0"] {
            place(temp.path(), version);
            let nested = temp.path().join(version).join("nested").join("deeper");
            std::fs::create_dir_all(&nested).unwrap();
            std::fs::write(nested.join("data"), b"regular").unwrap();
        }
        let report = prune_in(temp.path(), &[], false).unwrap();
        assert_eq!(report.removed, ["4.2.0", "4.10.0"]);
        assert!(report.skipped.is_empty());
        assert!(!temp.path().join("4.2.0").exists());
        assert!(!temp.path().join("4.10.0").exists());
    }
}
