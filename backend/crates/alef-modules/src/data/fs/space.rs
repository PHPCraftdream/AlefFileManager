// SPDX-License-Identifier: MIT OR Apache-2.0
//! Where a path of the application lies on the disk. What the user allowed is the path itself; what
//! he substituted is a place in the folder the runtime keeps for the stand-in, the same path inside
//! the same scope, so that nothing but the place differs and the application is never told it.
use std::{fs, io, path::PathBuf};

use alef_core::{
    registry::context::CallContext,
    security::permissions::{Permission, Reach},
    AlefError,
};

/// A path as the application knows it and where its bytes are.
#[derive(Debug, Clone)]
pub(super) struct Place {
    /// What the application calls the path; the answers are given in these terms.
    pub shown: PathBuf,
    /// Where the bytes are: the path itself, or its place in the stand-in.
    pub real: PathBuf,
    /// The folder of the stand-in for the scope of the path; it exists as long as the scope does.
    root: Option<PathBuf>,
}

impl Place {
    /// Makes the stand-in of the scope exist, empty at first: the scope is always a folder.
    pub(super) fn prepare(&self) -> io::Result<()> {
        match &self.root {
            Some(root) => fs::create_dir_all(root),
            None => Ok(()),
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

#[derive(Debug, Clone)]
pub(super) struct Space {
    /// Where the stand-ins of this application keep their content.
    shadow: PathBuf,
}

impl Space {
    pub(super) fn new(shadow: PathBuf) -> Self {
        Self { shadow }
    }

    /// Authorizes `path` for `permission` and says where it lies.
    pub(super) fn place(
        &self,
        ctx: &CallContext,
        permission: Permission,
        path: &str,
        reach: Reach,
    ) -> Result<Place, AlefError> {
        let authorized =
            ctx.permissions
                .authorize_at(permission, Some(path), &ctx.grants(), reach)?;
        Ok(match authorized.shadow {
            None => Place {
                real: authorized.path.clone(),
                shown: authorized.path,
                root: None,
            },
            Some(shadow) => {
                let root = self.shadow.join(scope_folder(&shadow.scope));
                Place {
                    real: root.join(&shadow.inside),
                    shown: authorized.path,
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
}
