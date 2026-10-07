// SPDX-License-Identifier: MIT OR Apache-2.0
//! Runtime path grants (dialogs, drops) layered on top of manifest scopes for one session.
use crate::security::scope::{
    canonical,
    path::{parts, same},
};
use crate::{AlefError, ErrorCode};
use std::{path::Path, sync::RwLock};

/// One granted path: a directory grants its whole subtree, a file only itself.
#[derive(Debug)]
struct Grant {
    parts: Vec<String>,
    recursive: bool,
}

impl Grant {
    fn allows(&self, candidate: &[String]) -> bool {
        let prefix = candidate.len() >= self.parts.len()
            && self.parts.iter().zip(candidate).all(|(a, b)| same(a, b));
        prefix && (self.recursive || candidate.len() == self.parts.len())
    }
}

/// Per-session runtime filesystem grants; read and write are independent.
#[derive(Debug, Default)]
pub struct Grants {
    read: RwLock<Vec<Grant>>,
    write: RwLock<Vec<Grant>>,
}

impl Grants {
    /// Creates an empty grant set.
    pub fn new() -> Self {
        Self::default()
    }

    /// Grants read access to a file, or to a directory and everything below it.
    pub fn grant_read(&self, path: &Path) -> Result<(), AlefError> {
        add(&self.read, path)
    }

    /// Grants write access to a file, or to a directory and everything below it.
    pub fn grant_write(&self, path: &Path) -> Result<(), AlefError> {
        add(&self.write, path)
    }

    /// Reports whether nothing has been granted.
    pub fn is_empty(&self) -> bool {
        let empty =
            |list: &RwLock<Vec<Grant>>| list.read().unwrap_or_else(|e| e.into_inner()).is_empty();
        empty(&self.read) && empty(&self.write)
    }

    /// Tests canonical candidate components against the read or write grants.
    pub(crate) fn allows(&self, write: bool, candidate: &[String]) -> bool {
        let list = if write { &self.write } else { &self.read };
        list.read()
            .unwrap_or_else(|e| e.into_inner())
            .iter()
            .any(|grant| grant.allows(candidate))
    }
}

fn add(list: &RwLock<Vec<Grant>>, path: &Path) -> Result<(), AlefError> {
    let canonical = canonical(path)
        .ok_or_else(|| AlefError::new(ErrorCode::InvalidArgument, "path cannot be granted"))?;
    let grant = Grant {
        parts: parts(&canonical),
        recursive: canonical.is_dir(),
    };
    list.write().unwrap_or_else(|e| e.into_inner()).push(grant);
    Ok(())
}
