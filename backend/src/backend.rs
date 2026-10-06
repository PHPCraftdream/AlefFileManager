// SPDX-License-Identifier: MIT OR Apache-2.0
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use alef_runtime::Commands;
use serde::{Deserialize, Serialize};
use serde_json::json;

use crate::storage::{Language, Storage};

pub async fn commands(root: &Path, storage: Storage) -> io::Result<Commands> {
    let root = Arc::new(tokio::fs::canonicalize(root).await?);
    if !tokio::fs::metadata(root.as_ref()).await?.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Filesystem root must be a directory",
        ));
    }
    let mut commands = Commands::new();
    commands.register("hello", |(): (), context| async move {
        let hello = json!({"message": "Hello from Rust!", "process_id": std::process::id(), "engine": "Servo 0.6.0"});
        context.emit("backend.greeting", &hello).await?;
        Ok(hello)
    })?;
    let preferences = storage.clone();
    commands.register("preferences.get", move |(): (), _context| {
        let storage = preferences.clone();
        async move {
            Ok(Preferences {
                language: storage.language().await?,
            })
        }
    })?;
    commands.register(
        "preferences.set",
        move |preferences: Preferences, _context| {
            let storage = storage.clone();
            async move {
                storage.set_language(preferences.language).await?;
                Ok(preferences)
            }
        },
    )?;
    commands.register("directory.list", move |query: DirectoryQuery, _context| {
        let root = root.clone();
        async move {
            let requested = query.path.unwrap_or_else(|| root.as_ref().clone());
            tokio::task::spawn_blocking(move || read_directory(root.as_ref(), &requested))
                .await
                .map_err(io::Error::other)?
        }
    })?;
    Ok(commands)
}

#[derive(Deserialize)]
struct DirectoryQuery {
    path: Option<PathBuf>,
}

#[derive(Deserialize, Serialize)]
struct Preferences {
    language: Language,
}

#[derive(Serialize)]
struct Entry {
    name: String,
    path: String,
    size: u64,
    is_dir: bool,
    is_file: bool,
    is_symlink: bool,
}

#[derive(Serialize)]
struct Directory {
    root: String,
    path: String,
    parent: Option<String>,
    entries: Vec<Entry>,
}

fn read_directory(root: &Path, requested: &Path) -> io::Result<Directory> {
    let path = if requested.is_absolute() {
        requested.to_path_buf()
    } else {
        root.join(requested)
    };
    let path = path.canonicalize()?;
    if !path.starts_with(root) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Directory access is limited to the configured root",
        ));
    }
    let mut entries = Vec::new();
    for entry in std::fs::read_dir(&path)? {
        let entry = entry?;
        let entry_path = entry.path();
        let metadata = std::fs::symlink_metadata(&entry_path)?;
        entries.push(Entry {
            name: entry.file_name().to_string_lossy().into_owned(),
            path: entry_path.to_string_lossy().into_owned(),
            size: metadata.len(),
            is_dir: metadata.is_dir(),
            is_file: metadata.is_file(),
            is_symlink: metadata.is_symlink(),
        });
    }
    entries.sort_unstable_by(|left, right| {
        right
            .is_dir
            .cmp(&left.is_dir)
            .then_with(|| left.name.cmp(&right.name))
    });
    let parent = path
        .parent()
        .filter(|parent| parent.starts_with(root))
        .map(|parent| parent.to_string_lossy().into_owned());
    Ok(Directory {
        root: root.to_string_lossy().into_owned(),
        path: path.to_string_lossy().into_owned(),
        parent,
        entries,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn lists_real_metadata_and_clamps_parent_to_root() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        std::fs::create_dir(temporary.path().join("z-folder")).expect("folder");
        std::fs::write(temporary.path().join("a-file"), b"sample").expect("file");
        let root = temporary.path().canonicalize().expect("root");
        let listing = read_directory(&root, &root).expect("listing");
        assert!(listing.parent.is_none());
        assert_eq!(
            listing
                .entries
                .iter()
                .map(|entry| entry.name.as_str())
                .collect::<Vec<_>>(),
            ["z-folder", "a-file"]
        );
        assert!(listing.entries[0].is_dir);
        assert!(listing.entries[1].is_file);
        assert_eq!(listing.entries[1].size, 6);
        let child = read_directory(&root, &root.join("z-folder")).expect("child listing");
        assert_eq!(child.parent, Some(root.to_string_lossy().into_owned()));
    }

    #[test]
    fn refuses_paths_outside_the_root() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        std::fs::create_dir(temporary.path().join("allowed")).expect("root");
        let root = temporary
            .path()
            .join("allowed")
            .canonicalize()
            .expect("root");
        assert_eq!(
            read_directory(&root, temporary.path())
                .err()
                .expect("denied")
                .kind(),
            io::ErrorKind::PermissionDenied
        );
        assert_eq!(
            read_directory(&root, Path::new(".."))
                .err()
                .expect("denied traversal")
                .kind(),
            io::ErrorKind::PermissionDenied
        );
    }

    #[tokio::test]
    async fn invalid_language_cannot_overwrite_durable_preferences() {
        let directory = tempfile::tempdir().expect("application directory");
        let database = directory.path().join("database");
        let storage = Storage::open(&database).await.expect("open");
        let commands = commands(directory.path(), storage).await.expect("commands");
        let bridge = alef_runtime::Bridge::new(Commands::new(), None, None)
            .await
            .expect("runtime");
        let context = bridge.handle();
        commands
            .invoke(
                "preferences.set",
                json!({"language": "he"}),
                context.clone(),
            )
            .await
            .expect("Hebrew");
        let invalid = commands
            .invoke(
                "preferences.set",
                json!({"language": "invalid"}),
                context.clone(),
            )
            .await;
        assert_eq!(
            invalid.expect_err("invalid language rejected").kind(),
            io::ErrorKind::InvalidInput
        );
        assert_eq!(
            commands
                .invoke("preferences.get", serde_json::Value::Null, context)
                .await
                .expect("preferences"),
            json!({"language": "he"})
        );
        tokio::task::spawn_blocking(move || drop(commands))
            .await
            .expect("close handles");
        bridge.shutdown().await.expect("runtime shutdown");
        let reopened = Storage::open(&database).await.expect("reopen");
        assert_eq!(
            reopened.language().await.expect("persisted language"),
            Language::He
        );
    }
}
