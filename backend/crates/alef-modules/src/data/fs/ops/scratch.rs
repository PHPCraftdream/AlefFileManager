// SPDX-License-Identifier: MIT OR Apache-2.0
//! The temp file or folder of a session. It is removed when the session closes (the resources of a
//! session close in reverse order, so whatever was opened on it is closed first) and when the
//! owner is dropped without that (a refused insertion, a cancelled call). Best effort: what a page
//! moved away is not touched, what a crash leaves stays, and a link is removed as a link.
use std::{
    fs,
    future::Future,
    path::{Path, PathBuf},
    pin::Pin,
};

use alef_core::{session::Resource, AlefError};

use super::super::fault::fault;

pub(in crate::data::fs) struct Scratch {
    target: Option<Target>,
}

struct Target {
    path: PathBuf,
    directory: bool,
}

impl Scratch {
    pub(in crate::data::fs) fn create(folder: &Path, directory: bool) -> Result<Self, AlefError> {
        fs::create_dir_all(folder).map_err(fault)?;
        let mut builder = tempfile::Builder::new();
        builder.prefix("alef-");
        let path = if directory {
            builder.tempdir_in(folder).map_err(fault)?.keep()
        } else {
            let (_, path) = builder
                .tempfile_in(folder)
                .map_err(fault)?
                .keep()
                .map_err(|error| fault(error.error))?;
            path
        };
        Ok(Self {
            target: Some(Target { path, directory }),
        })
    }

    pub(in crate::data::fs) fn path(&self) -> &Path {
        self.target
            .as_ref()
            .map_or(Path::new(""), |target| &target.path)
    }
}

impl Target {
    fn remove(self) {
        // std does not follow a link, neither the one inside a folder nor one in place of it.
        let _ = if self.directory {
            fs::remove_dir_all(&self.path)
        } else {
            fs::remove_file(&self.path)
        };
    }
}

impl Resource for Scratch {
    fn close(mut self: Box<Self>) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        let target = self.target.take();
        Box::pin(async move {
            if let Some(target) = target {
                let _ = tokio::task::spawn_blocking(move || target.remove()).await;
            }
        })
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        if let Some(target) = self.target.take() {
            target.remove();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn dropping_the_owner_removes_a_file_and_a_folder_with_what_it_holds() {
        let folder = tempfile::tempdir().unwrap();
        let file = Scratch::create(folder.path(), false).unwrap();
        let directory = Scratch::create(folder.path(), true).unwrap();
        fs::write(directory.path().join("note"), b"x").unwrap();
        let file_path = file.path().to_path_buf();
        let directory_path = directory.path().to_path_buf();
        assert!(file_path.is_file() && directory_path.is_dir());
        for path in [&file_path, &directory_path] {
            assert!(path
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("alef-"));
        }
        drop((file, directory));
        assert!(!file_path.exists() && !directory_path.exists());
    }

    #[tokio::test]
    async fn closing_the_owner_has_removed_it_when_the_future_ends() {
        let folder = tempfile::tempdir().unwrap();
        let scratch = Scratch::create(folder.path(), true).unwrap();
        let path = scratch.path().to_path_buf();
        Box::new(scratch).close().await;
        assert!(!path.exists());
    }

    #[test]
    fn an_entry_that_is_gone_is_no_error() {
        let folder = tempfile::tempdir().unwrap();
        let scratch = Scratch::create(folder.path(), false).unwrap();
        fs::remove_file(scratch.path()).unwrap();
        drop(scratch);
    }

    #[test]
    fn what_was_moved_away_is_left_alone() {
        let folder = tempfile::tempdir().unwrap();
        let scratch = Scratch::create(folder.path(), false).unwrap();
        let kept = folder.path().join("kept");
        fs::write(scratch.path(), b"data").unwrap();
        fs::rename(scratch.path(), &kept).unwrap();
        drop(scratch);
        assert_eq!(fs::read(&kept).unwrap(), b"data");
    }
}
