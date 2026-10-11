// SPDX-License-Identifier: MIT OR Apache-2.0
//! R-008: document-owned scratch paths are reclaimed without deleting somebody else's data.
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use alef_core::{security::consent::Decision, session::Session, ErrorCode};
use serde_json::{json, Value};
use tokio::time::timeout;

use crate::{at, common::Fixture, fixture_in, read, write};

const BOUND: Duration = Duration::from_secs(5);

// The fixture's APPCACHE is shared and outside its TempDir. Own only the paths this test
// creates, including replacements, and clean them even when an assertion unwinds.
#[derive(Default)]
struct Scratch(Vec<PathBuf>);
impl Scratch {
    async fn create(&mut self, app: &Fixture, session: &Arc<Session>, directory: bool) -> PathBuf {
        let command = if directory {
            "fs.tempDir"
        } else {
            "fs.tempFile"
        };
        let value = app.call_as(session, command, Value::Null).await.unwrap();
        let path = PathBuf::from(value.as_str().unwrap());
        self.0.push(path.clone());
        assert!(if directory {
            path.is_dir()
        } else {
            path.is_file()
        });
        path
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        for path in self.0.iter().rev() {
            let Ok(metadata) = fs::symlink_metadata(path) else {
                continue;
            };
            let result = if metadata.file_type().is_symlink() {
                #[cfg(windows)]
                {
                    if metadata.is_dir() {
                        fs::remove_dir(path)
                    } else {
                        fs::remove_file(path)
                    }
                }
                #[cfg(not(windows))]
                {
                    fs::remove_file(path)
                }
            } else if metadata.is_dir() {
                // std's recursive removal does not traverse directory junctions/symlinks.
                fs::remove_dir_all(path)
            } else {
                fs::remove_file(path)
            };
            if let Err(error) = result {
                eprintln!(
                    "test scratch cleanup failed for {}: {error}",
                    path.display()
                );
            }
        }
    }
}

async fn close(session: &Session) {
    timeout(BOUND, session.close())
        .await
        .expect("session close must finish within five seconds");
    assert!(!session.is_open());
}

/// The closing of a session has removed these when it ends, not some time later.
fn deleted(paths: &[&Path]) {
    for path in paths {
        match fs::symlink_metadata(path) {
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {}
            Ok(_) => panic!("{} is still there", path.display()),
            Err(error) => panic!("cannot inspect scratch path {}: {error}", path.display()),
        }
    }
}

#[tokio::test]
async fn a_temporary_file_is_deleted_when_its_session_closes() {
    let directory = tempfile::tempdir().unwrap();
    let app = fixture_in(directory.path(), Decision::Allow).await;
    let mut scratch = Scratch::default();
    let file = scratch.create(&app, &app.session(), false).await;
    write(&app, &file, b"scratch").await.unwrap();
    assert_eq!(read(&app, &file).await.unwrap(), b"scratch");
    close(&app.session()).await;
    deleted(&[&file]);
}

#[tokio::test]
async fn a_temporary_directory_and_its_nested_contents_are_deleted_when_its_session_closes() {
    let directory = tempfile::tempdir().unwrap();
    let app = fixture_in(directory.path(), Decision::Allow).await;
    let mut scratch = Scratch::default();
    let root = scratch.create(&app, &app.session(), true).await;
    let nested = root.join("one/two");
    app.call(
        "fs.mkdir",
        json!({"path": nested.to_string_lossy(), "recursive": true}),
    )
    .await
    .unwrap();
    let file = nested.join("note");
    write(&app, &file, b"nested").await.unwrap();
    close(&app.session()).await;
    deleted(&[&root, &nested, &file]);
}

#[tokio::test]
async fn a_temporary_path_the_page_moved_away_survives_the_close() {
    let directory = tempfile::tempdir().unwrap();
    let app = fixture_in(directory.path(), Decision::Allow).await;
    let mut scratch = Scratch::default();
    let mut saved = Vec::new();
    for is_dir in [false, true] {
        let source = scratch.create(&app, &app.session(), is_dir).await;
        let destination = directory.path().join(if is_dir {
            "saved-directory"
        } else {
            "saved-file"
        });
        let original = if is_dir {
            source.join("original")
        } else {
            source.clone()
        };
        write(&app, &original, b"original").await.unwrap();
        app.call(
            "fs.rename",
            json!({"from": source.to_string_lossy(), "to": destination.to_string_lossy()}),
        )
        .await
        .unwrap();
        saved.push(if is_dir {
            destination.join("original")
        } else {
            destination
        });
    }
    close(&app.session()).await;
    for path in saved {
        assert_eq!(fs::read(path).unwrap(), b"original");
    }
}

#[tokio::test]
async fn deleting_temporary_paths_before_close_does_not_prevent_other_cleanup() {
    let directory = tempfile::tempdir().unwrap();
    let app = fixture_in(directory.path(), Decision::Allow).await;
    let mut scratch = Scratch::default();
    let file = scratch.create(&app, &app.session(), false).await;
    let root = scratch.create(&app, &app.session(), true).await;
    let remaining = scratch.create(&app, &app.session(), false).await;
    app.call("fs.remove", at(&file)).await.unwrap();
    app.call(
        "fs.remove",
        json!({"path": root.to_string_lossy(), "recursive": true}),
    )
    .await
    .unwrap();
    deleted(&[&file, &root]);
    close(&app.session()).await;
    deleted(&[&file, &root, &remaining]);
}

#[tokio::test]
async fn an_open_temporary_file_handle_does_not_block_session_close_or_cleanup() {
    let directory = tempfile::tempdir().unwrap();
    let app = fixture_in(directory.path(), Decision::Allow).await;
    let mut scratch = Scratch::default();
    let file = scratch.create(&app, &app.session(), false).await;
    write(&app, &file, b"open").await.unwrap();
    let handle = app
        .call(
            "fs.open",
            json!({"path": file.to_string_lossy(), "read": true, "write": true}),
        )
        .await
        .unwrap();
    assert!(!handle.is_null());
    // Deliberately do not call fs.close: session teardown must release the handle itself.
    close(&app.session()).await;
    deleted(&[&file]);
}

#[tokio::test]
async fn closing_one_session_deletes_only_its_temporary_paths() {
    let directory = tempfile::tempdir().unwrap();
    let app = fixture_in(directory.path(), Decision::Allow).await;
    let other = app.open_window(2).await;
    let mut scratch = Scratch::default();
    let first = scratch.create(&app, &app.session(), false).await;
    let second = scratch.create(&app, &other, true).await;
    fs::write(second.join("note"), b"other session").unwrap();
    assert_eq!(
        app.call_as(&other, "fs.stat", at(&first))
            .await
            .unwrap_err()
            .code,
        ErrorCode::PermissionDenied
    );
    assert_eq!(
        app.call("fs.stat", at(&second)).await.unwrap_err().code,
        ErrorCode::PermissionDenied
    );
    close(&app.session()).await;
    deleted(&[&first]);
    assert!(other.is_open());
    assert_eq!(
        app.call_as(&other, "fs.stat", at(&second)).await.unwrap()["kind"],
        "dir"
    );
    assert_eq!(fs::read(second.join("note")).unwrap(), b"other session");
    close(&other).await;
    deleted(&[&second]);
}

#[tokio::test]
async fn reloading_the_document_removes_its_temporary_path_grant_even_for_a_replacement() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = fixture_in(directory.path(), Decision::Allow).await;
    let mut scratch = Scratch::default();
    let file = scratch.create(&app, &app.session(), false).await;
    assert_eq!(
        app.call("fs.stat", at(&file)).await.unwrap()["kind"],
        "file"
    );
    timeout(BOUND, app.reload_document())
        .await
        .expect("reload must close the previous session within five seconds");
    deleted(&[&file]);
    fs::write(&file, b"replacement").unwrap();
    assert_eq!(
        app.call("fs.stat", at(&file)).await.unwrap_err().code,
        ErrorCode::PermissionDenied
    );
    assert_eq!(fs::read(&file).unwrap(), b"replacement");
    close(&app.session()).await;
    assert_eq!(fs::read(&file).unwrap(), b"replacement");
}

fn directory_link(target: &Path, link: &Path) {
    #[cfg(windows)]
    {
        let output = std::process::Command::new("cmd")
            .args(["/C", "mklink", "/J"])
            .arg(link)
            .arg(target)
            .output()
            .expect("cmd mklink /J must be available on Windows");
        assert!(
            output.status.success(),
            "mandatory junction creation failed: stdout={} stderr={}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
    }
    #[cfg(unix)]
    std::os::unix::fs::symlink(target, link).expect("directory symlink must be available on Unix");
}

#[tokio::test]
async fn cleanup_of_a_temporary_directory_does_not_follow_an_inner_directory_link() {
    let directory = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let target = outside.path().join("target");
    fs::create_dir(&target).unwrap();
    fs::write(target.join("keep"), b"outside").unwrap();
    let app = fixture_in(directory.path(), Decision::Allow).await;
    let mut scratch = Scratch::default();
    let root = scratch.create(&app, &app.session(), true).await;
    directory_link(&target, &root.join("link"));
    write(&app, &root.join("local"), b"local").await.unwrap();
    close(&app.session()).await;
    deleted(&[&root]);
    assert_eq!(fs::read(target.join("keep")).unwrap(), b"outside");
    assert_eq!(fs::read_dir(&target).unwrap().count(), 1);
}

#[tokio::test]
async fn cleanup_of_a_temporary_directory_does_not_follow_an_optional_inner_file_symlink() {
    let directory = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let target = outside.path().join("keep");
    fs::write(&target, b"outside").unwrap();
    let app = fixture_in(directory.path(), Decision::Allow).await;
    let mut scratch = Scratch::default();
    let root = scratch.create(&app, &app.session(), true).await;
    #[cfg(windows)]
    let result = std::os::windows::fs::symlink_file(&target, root.join("link"));
    #[cfg(unix)]
    let result = std::os::unix::fs::symlink(&target, root.join("link"));
    if let Err(error) = result {
        if error.kind() == std::io::ErrorKind::PermissionDenied {
            eprintln!("skipped optional file symlink: PermissionDenied: {error}");
            close(&app.session()).await;
            return;
        }
        panic!("file symlink creation failed: {error}");
    }
    close(&app.session()).await;
    deleted(&[&root]);
    assert_eq!(fs::read(&target).unwrap(), b"outside");
}

#[tokio::test]
async fn replacing_a_temporary_directory_root_with_a_link_keeps_the_target_safe() {
    let directory = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("keep"), b"outside").unwrap();
    let app = fixture_in(directory.path(), Decision::Allow).await;
    let mut scratch = Scratch::default();
    let root = scratch.create(&app, &app.session(), true).await;
    fs::remove_dir(&root).unwrap();
    directory_link(outside.path(), &root);
    close(&app.session()).await;
    // The link goes, what it leads to stays.
    assert_eq!(fs::read(outside.path().join("keep")).unwrap(), b"outside");
    assert_eq!(fs::read_dir(outside.path()).unwrap().count(), 1);
}
