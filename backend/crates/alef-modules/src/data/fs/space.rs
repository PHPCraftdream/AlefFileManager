// SPDX-License-Identifier: MIT OR Apache-2.0
//! Where a path of the application lies on the disk. What the user allowed is the path itself; what
//! he substituted is a place in the folder the runtime keeps for the stand-in, the same path inside
//! the same scope, so that nothing but the place differs and the application is never told it.
use std::{
    fs, io,
    path::{Path, PathBuf},
};

use alef_core::{
    registry::context::CallContext,
    security::permissions::{Grants, Permission, PermissionSet, Reach},
    AlefError,
};

/// A path as the application knows it and where its bytes are.
#[derive(Debug, Clone)]
pub(in crate::data) struct Place {
    /// The path as the application spelled it; the answers are given in these terms.
    pub shown: PathBuf,
    /// Where the bytes are: the path itself, or its place in the stand-in.
    pub real: PathBuf,
    /// The folder of the stand-in for the scope of the path; it exists as long as the scope does.
    root: Option<PathBuf>,
}

impl Place {
    #[cfg(test)]
    pub(super) fn for_test(shown: PathBuf, real: PathBuf, root: Option<PathBuf>) -> Place {
        Place { shown, real, root }
    }

    /// Makes the stand-in of the scope exist, empty at first: the scope is always a folder.
    pub(in crate::data) fn prepare(&self) -> io::Result<()> {
        match &self.root {
            Some(root) => fs::create_dir_all(root),
            None => Ok(()),
        }
    }

    /// What the application calls a path the disk reports for this place: the path itself, or, for a
    /// stand-in, the same place inside the scope; `None` for a path the place does not hold.
    pub(super) fn shown_path(&self, found: &Path) -> Option<PathBuf> {
        match found.strip_prefix(&self.real) {
            Ok(inside) => Some(self.shown.join(inside)),
            // A real place may be reported in another spelling of the same path: better told than lost.
            Err(_) if self.root.is_none() => Some(found.to_path_buf()),
            Err(_) => None,
        }
    }

    /// The place of an entry of this folder.
    pub(super) fn shown_entry(&self, name: &str) -> PathBuf {
        self.shown.join(name)
    }
}

/// A short name for a scope, fit for a folder name and different for different scopes.
fn scope_folder(scope: &str) -> String {
    let readable: String = scope
        .chars()
        .map(|c| if c.is_ascii_alphanumeric() { c } else { '_' })
        .take(24)
        .collect();
    let hash = scope.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    });
    format!("{readable}-{hash:016x}")
}

/// The stand-in root in its physical spelling: the opened-handle check compares the path of the
/// created file against this place, and the system reports long names and resolved links.
fn canonical_root(root: &Path) -> Option<PathBuf> {
    fs::create_dir_all(root).ok()?;
    Some(strip_verbatim(fs::canonicalize(root).ok()?))
}

fn strip_verbatim(path: PathBuf) -> PathBuf {
    #[cfg(windows)]
    {
        let text = path.to_string_lossy();
        if let Some(rest) = text.strip_prefix(r"\\?\UNC\") {
            return PathBuf::from(format!(r"\\{rest}"));
        }
        if let Some(rest) = text.strip_prefix(r"\\?\") {
            return PathBuf::from(rest);
        }
    }
    path
}

#[derive(Debug, Clone)]
pub(in crate::data) struct Space {
    /// Where the stand-ins of this application keep their content.
    shadow: PathBuf,
}

impl Space {
    pub(in crate::data) fn new(shadow: PathBuf) -> Self {
        Self { shadow }
    }

    /// Authorizes `path` for `permission` and says where it lies: the platform calls run off the
    /// poll.
    pub(in crate::data) async fn place(
        &self,
        ctx: &CallContext,
        permission: Permission,
        path: &str,
        reach: Reach,
    ) -> Result<Place, AlefError> {
        let permissions = ctx.permissions.clone();
        let grants = ctx.grants();
        let shadow = self.shadow.clone();
        let path = path.to_owned();
        super::blocking(move || {
            Self::place_sync(&shadow, &permissions, &grants, permission, &path, reach)
        })
        .await
    }
}

impl Space {
    /// The synchronous core of [`Space::place`]: authorize, and resolve the stand-in.
    fn place_sync(
        shadow_root: &Path,
        permissions: &PermissionSet,
        grants: &Grants,
        permission: Permission,
        path: &str,
        reach: Reach,
    ) -> Result<Place, AlefError> {
        let authorized = permissions.authorize_at(permission, Some(path), grants, reach)?;
        Ok(match authorized.shadow {
            None => Place {
                real: authorized.path,
                shown: PathBuf::from(path),
                root: None,
            },
            Some(shadow) => {
                let folder = shadow_root.join(scope_folder(&shadow.scope));
                let root = canonical_root(&folder).unwrap_or(folder);
                Place {
                    real: root.join(&shadow.inside),
                    shown: PathBuf::from(path),
                    root: Some(root),
                }
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn different_scopes_get_different_folders_with_names_that_are_safe() {
        let a = scope_folder("$DOCUMENTS/**");
        let b = scope_folder("$DOCUMENTS/*");
        assert_ne!(a, b);
        assert_eq!(a, scope_folder("$DOCUMENTS/**"));
        for name in [a, b, scope_folder(r"C:\a b\..\**"), scope_folder("")] {
            assert!(
                name.chars()
                    .all(|c| c.is_ascii_alphanumeric() || c == '_' || c == '-'),
                "{name}"
            );
        }
    }

    #[test]
    fn the_stand_in_root_is_canonicalized_through_links_so_the_opened_path_matches() {
        let base = tempfile::tempdir().expect("temporary folder");
        let physical = base.path().join("physical");
        let inner = physical.join("inner");
        fs::create_dir_all(&inner).expect("folders");
        let linked = base.path().join("linked");
        #[cfg(windows)]
        let made = std::process::Command::new("cmd")
            .args([
                "/C",
                "mklink",
                "/J",
                &linked.to_string_lossy(),
                &physical.to_string_lossy(),
            ])
            .status()
            .map(|status| status.success())
            .unwrap_or(false);
        #[cfg(unix)]
        let made = std::os::unix::fs::symlink(&physical, &linked).is_ok();
        if !made {
            return;
        }
        let expected = strip_verbatim(fs::canonicalize(&inner).expect("canonical"));
        assert_eq!(
            canonical_root(&linked.join("inner")).expect("root"),
            expected
        );
    }
}
