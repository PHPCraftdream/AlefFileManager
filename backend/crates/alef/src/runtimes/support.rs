// SPDX-License-Identifier: MIT OR Apache-2.0
//! Test-owned scratch folders, independent of environment mutation and dependencies.
use std::{
    io,
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

static NEXT: AtomicU64 = AtomicU64::new(0);

pub(super) struct Scratch(PathBuf);

impl Scratch {
    pub(super) fn new() -> Self {
        loop {
            // Relaxed only allocates names; it does not publish shared data.
            let unique = NEXT.fetch_add(1, Ordering::Relaxed);
            let path =
                std::env::temp_dir().join(format!("alef-runtimes-{}-{unique}", std::process::id()));
            match std::fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(error) if error.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("scratch directory: {error}"),
            }
        }
    }

    pub(super) fn path(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
