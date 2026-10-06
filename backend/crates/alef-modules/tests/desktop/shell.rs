// SPDX-License-Identifier: MIT OR Apache-2.0
//! `shell` through the registry: what the manifest allows reaches the desktop, and nothing else. The
//! last test moves files to the real trash and runs only on request (`ALEF_TEST_DESKTOP=1 cargo test
//! -p alef-modules --test shell -- --ignored`; CI does on its clean runners). Opening and showing are
//! never tried against the real desktop: they would put windows in front of somebody.

use std::{fs, path::Path};

use crate::common::{desktop_asked, Fixture};
use alef_core::ErrorCode;
use alef_modules::{ShellBackend, ShellRequest, SystemShell};
use serde_json::json;

const MANIFEST: &str = include_str!("../fixtures/app.ktav");

/// URLs `https://example.com/docs/**` allowed, files read and written under `$TEMP`.
fn open_manifest(read: bool, write: bool) -> String {
    let mut text = MANIFEST.replace('\r', "");
    text = text.replace(
        "        openExternal: []",
        "        openExternal: [ https://example.com/docs/* ]",
    );
    let scope = |on: bool| if on { "[ $TEMP/** ]" } else { "[]" };
    text = text.replace(
        "        read: []\n        write: []",
        &format!(
            "        read: {}\n        write: {}",
            scope(read),
            scope(write)
        ),
    );
    assert!(text.contains("https://example.com/docs/*"));
    text
}

fn workdir(fixture: &Fixture) -> tempfile::TempDir {
    fs::create_dir_all(&fixture.context.paths.temp).unwrap();
    tempfile::Builder::new()
        .prefix("shell-")
        .tempdir_in(&fixture.context.paths.temp)
        .unwrap()
}

fn text(path: &Path) -> String {
    fs::canonicalize(path)
        .unwrap()
        .to_string_lossy()
        .trim_start_matches(r"\\?\")
        .to_owned()
}

fn asked(fixture: &Fixture) -> Vec<ShellRequest> {
    fixture.shell.asked()
}

#[tokio::test]
async fn an_address_inside_the_scope_is_opened_and_one_outside_never_reaches_the_desktop() {
    let fixture = Fixture::new(Some(&open_manifest(false, false)), &[]).await;
    let allowed = "https://example.com/docs/guide?page=2#top";
    fixture
        .call("shell.openExternal", json!({ "url": allowed }))
        .await
        .expect("allowed");
    assert_eq!(
        asked(&fixture),
        [ShellRequest {
            operation: "openExternal",
            target: allowed.to_owned()
        }]
    );
    for url in [
        "https://example.com/private",
        "https://example.com.evil.net/docs/x",
        "https://evil.net/docs/x",
        "http://example.com/docs/x",
        "https://user@example.com/docs/x",
        "https://example.com/docs/../private",
        "https://example.com/docs/%2e%2e/private",
        "file:///etc/passwd",
        "javascript:alert(1)",
        "https://example.com/docs/x y",
        "",
    ] {
        let error = fixture
            .call("shell.openExternal", json!({ "url": url }))
            .await
            .expect_err(url);
        assert_eq!(error.code, ErrorCode::PermissionDenied, "{url}");
    }
    assert_eq!(
        asked(&fixture).len(),
        1,
        "nothing but the allowed address got through"
    );
}

#[tokio::test]
async fn without_a_scope_no_address_is_opened() {
    let fixture = Fixture::new(None, &[]).await;
    let error = fixture
        .call(
            "shell.openExternal",
            json!({ "url": "https://example.com/docs/x" }),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::PermissionDenied);
    assert!(asked(&fixture).is_empty());
}

#[tokio::test]
async fn a_file_in_the_read_scope_is_opened_shown_and_handed_over_by_its_real_path() {
    let fixture = Fixture::new(Some(&open_manifest(true, false)), &[]).await;
    let directory = workdir(&fixture);
    let note = directory.path().join("note.txt");
    fs::write(&note, "hello").unwrap();
    let detour = directory.path().join(".").join("note.txt");
    fixture
        .call("shell.openPath", json!({ "path": note.to_string_lossy() }))
        .await
        .expect("open");
    fixture
        .call(
            "shell.showInFolder",
            json!({ "path": detour.to_string_lossy() }),
        )
        .await
        .expect("show");
    fixture
        .call(
            "shell.openPath",
            json!({ "path": directory.path().to_string_lossy() }),
        )
        .await
        .expect("a folder opens too");
    let canonical = text(&note);
    assert_eq!(
        asked(&fixture),
        [
            ShellRequest {
                operation: "openPath",
                target: canonical.clone()
            },
            ShellRequest {
                operation: "showInFolder",
                target: canonical
            },
            ShellRequest {
                operation: "openPath",
                target: text(directory.path())
            },
        ],
        "the desktop is given the path that was checked, not the one the document wrote"
    );
}

#[tokio::test]
async fn a_path_outside_the_scope_or_without_the_right_is_denied() {
    let readable = Fixture::new(Some(&open_manifest(true, false)), &[]).await;
    let outside = tempfile::tempdir().unwrap();
    let stray = outside.path().join("stray.txt");
    fs::write(&stray, "x").unwrap();
    for command in ["shell.openPath", "shell.showInFolder", "shell.trash"] {
        let error = readable
            .call(command, json!({ "path": stray.to_string_lossy() }))
            .await
            .expect_err(command);
        assert_eq!(
            error.code,
            ErrorCode::PermissionDenied,
            "{command} outside the scope"
        );
    }
    let directory = workdir(&readable);
    let note = directory.path().join("note.txt");
    fs::write(&note, "x").unwrap();
    let error = readable
        .call("shell.trash", json!({ "path": note.to_string_lossy() }))
        .await
        .unwrap_err();
    assert_eq!(
        error.code,
        ErrorCode::PermissionDenied,
        "reading a file is not the right to delete it"
    );
    let nothing = Fixture::new(None, &[]).await;
    let error = nothing
        .call("shell.openPath", json!({ "path": note.to_string_lossy() }))
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::PermissionDenied);
    assert!(asked(&readable).is_empty() && asked(&nothing).is_empty());
}

#[tokio::test]
async fn a_relative_or_looping_path_cannot_reach_what_the_scope_does_not_cover() {
    let fixture = Fixture::new(Some(&open_manifest(true, true)), &[]).await;
    let directory = workdir(&fixture);
    // The folder above the scope (what `shell-x/../..` reaches) is not covered by it.
    let above = fixture.context.paths.temp.parent().unwrap().to_owned();
    let stray = above.join("stray.txt");
    fs::write(&stray, "x").unwrap();
    let climbing = directory.path().join("..").join("..").join("stray.txt");
    for path in ["note.txt", "./note.txt", "..", ""] {
        let error = fixture
            .call("shell.trash", json!({ "path": path }))
            .await
            .expect_err(path);
        assert_eq!(error.code, ErrorCode::PermissionDenied, "{path:?}");
    }
    let error = fixture
        .call("shell.trash", json!({ "path": climbing.to_string_lossy() }))
        .await
        .expect_err("a path that climbs out of the scope");
    assert_eq!(error.code, ErrorCode::PermissionDenied);
    assert!(asked(&fixture).is_empty());
    assert!(stray.exists());
    fs::remove_file(&stray).unwrap();
}

#[tokio::test]
async fn trash_takes_what_the_write_scope_covers_and_a_missing_path_is_not_found() {
    let fixture = Fixture::new(Some(&open_manifest(true, true)), &[]).await;
    let directory = workdir(&fixture);
    let old = directory.path().join("old.txt");
    fs::write(&old, "x").unwrap();
    fixture
        .call("shell.trash", json!({ "path": old.to_string_lossy() }))
        .await
        .expect("trash");
    assert_eq!(
        asked(&fixture),
        [ShellRequest {
            operation: "trash",
            target: text(&old)
        }]
    );
    let missing = directory.path().join("never-was.txt");
    for command in ["shell.openPath", "shell.showInFolder", "shell.trash"] {
        let error = fixture
            .call(command, json!({ "path": missing.to_string_lossy() }))
            .await
            .expect_err(command);
        assert_eq!(error.code, ErrorCode::NotFound, "{command}");
    }
    assert_eq!(
        asked(&fixture).len(),
        1,
        "a path that is not there is not passed on"
    );
}

#[tokio::test]
async fn opening_a_path_never_starts_a_program() {
    let fixture = Fixture::new(Some(&open_manifest(true, true)), &[]).await;
    let directory = workdir(&fixture);
    for name in [
        "setup.exe",
        "run.BAT",
        "do.cmd",
        "x.ps1",
        "x.vbs",
        "x.js",
        "x.msi",
        "link.lnk",
        "site.url",
        "x.hta",
        "x.jar",
        "Tool.app",
        "x.command",
        "x.sh",
        "x.pkg",
        "x.desktop",
    ] {
        let file = directory.path().join(name);
        fs::write(&file, "x").unwrap();
        let error = fixture
            .call("shell.openPath", json!({ "path": file.to_string_lossy() }))
            .await
            .expect_err(name);
        assert_eq!(error.code, ErrorCode::PermissionDenied, "{name}");
    }
    assert!(asked(&fixture).is_empty());
    for name in [
        "note.txt",
        "photo.png",
        "archive.zip",
        "page.html",
        "no-extension",
    ] {
        let file = directory.path().join(name);
        fs::write(&file, "x").unwrap();
        fixture
            .call("shell.openPath", json!({ "path": file.to_string_lossy() }))
            .await
            .unwrap_or_else(|error| panic!("{name}: {error}"));
    }
    assert_eq!(asked(&fixture).len(), 5);
    let program = directory.path().join("setup.exe");
    fixture
        .call(
            "shell.showInFolder",
            json!({ "path": program.to_string_lossy() }),
        )
        .await
        .expect("showing a program is not running it");
    fixture
        .call("shell.trash", json!({ "path": program.to_string_lossy() }))
        .await
        .expect("trashing a program is not running it");
}

#[cfg(unix)]
#[tokio::test]
async fn a_file_with_an_execute_bit_is_not_opened() {
    use std::os::unix::fs::PermissionsExt;
    let fixture = Fixture::new(Some(&open_manifest(true, true)), &[]).await;
    let directory = workdir(&fixture);
    let tool = directory.path().join("tool");
    fs::write(&tool, "#!/bin/sh\n").unwrap();
    fs::set_permissions(&tool, fs::Permissions::from_mode(0o755)).unwrap();
    let error = fixture
        .call("shell.openPath", json!({ "path": tool.to_string_lossy() }))
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::PermissionDenied);
    fs::set_permissions(&tool, fs::Permissions::from_mode(0o644)).unwrap();
    fixture
        .call("shell.openPath", json!({ "path": tool.to_string_lossy() }))
        .await
        .expect("without the bit it is a text file");
}

#[tokio::test]
async fn unknown_fields_and_wrong_shapes_are_refused() {
    let fixture = Fixture::new(Some(&open_manifest(true, true)), &[]).await;
    for (command, body) in [
        ("shell.openExternal", json!({})),
        ("shell.openExternal", json!({ "url": 1 })),
        (
            "shell.openExternal",
            json!({ "url": "https://example.com/docs/x", "extra": 1 }),
        ),
        ("shell.openPath", json!({ "path": ["a"] })),
        ("shell.trash", json!(null)),
    ] {
        let error = fixture
            .call(command, body.clone())
            .await
            .expect_err(command);
        assert_eq!(error.code, ErrorCode::InvalidArgument, "{command} {body}");
    }
    assert!(asked(&fixture).is_empty());
}

#[test]
#[ignore = "moves a file to the trash of the desktop"]
fn the_system_trash_takes_a_file_and_a_folder() {
    if !desktop_asked() {
        eprintln!("skipped: ALEF_TEST_DESKTOP=1 allows the test to fill the trash");
        return;
    }
    let directory = tempfile::tempdir().expect("a scratch folder");
    let file = directory.path().join("alef-trash-test-file.txt");
    let folder = directory.path().join("alef-trash-test-folder");
    std::fs::write(&file, "x").unwrap();
    std::fs::create_dir(&folder).unwrap();
    std::fs::write(folder.join("inside.txt"), "y").unwrap();
    let shell = SystemShell;
    shell.trash(&file).expect("trash a file");
    shell.trash(&folder).expect("trash a folder");
    assert!(!file.exists(), "the file left its place");
    assert!(!folder.exists(), "the folder left its place");
}
