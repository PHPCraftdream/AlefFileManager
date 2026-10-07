// SPDX-License-Identifier: MIT OR Apache-2.0
//! What `fs` lets an application reach, and what it gives where the user chose a stand-in.
use std::{fs, path::Path};

use alef_core::{security::consent::Decision, ErrorCode};
use serde_json::{json, Value};

use crate::{at, fixture, fixture_in, read, scope_of, write};

#[cfg(any(unix, windows))]
fn link(target: &Path, at: &Path) -> bool {
    #[cfg(unix)]
    let made = std::os::unix::fs::symlink(target, at);
    #[cfg(windows)]
    let made = std::os::windows::fs::symlink_file(target, at);
    made.is_ok()
}

async fn denied(app: &common_fixture::Fixture, command: &str, args: Value) {
    let error = app.call_reply(command, args, None).await.unwrap_err();
    assert_eq!(
        error.code,
        ErrorCode::PermissionDenied,
        "{command}: {error}"
    );
}

mod common_fixture {
    pub use crate::common::Fixture;
}

#[tokio::test]
async fn nothing_outside_the_scopes_is_reached_and_every_refusal_is_the_same_refusal() {
    let outside = tempfile::tempdir().unwrap();
    let scope = tempfile::tempdir().unwrap();
    fs::write(outside.path().join("secret.txt"), "s").unwrap();
    let app = fixture_in(scope.path(), Decision::Allow).await;

    let secret = outside.path().join("secret.txt");
    let sneaking = scope
        .path()
        .join("..")
        .join(outside.path().file_name().unwrap())
        .join("secret.txt");
    for path in [&secret, &sneaking, Path::new("relative.txt"), Path::new("")] {
        denied(&app, "fs.readFile", at(path)).await;
        denied(&app, "fs.stat", at(path)).await;
        denied(&app, "fs.readDir", at(path)).await;
        denied(&app, "fs.exists", at(path)).await;
        denied(&app, "fs.lstat", at(path)).await;
        denied(&app, "fs.mkdir", at(path)).await;
        denied(&app, "fs.remove", at(path)).await;
    }
    let error = write(&app, &secret, b"x").await.unwrap_err();
    assert_eq!(error.code, ErrorCode::PermissionDenied);
    assert_eq!(fs::read(&secret).unwrap(), b"s", "nothing was written");
    // Both ends of a rename and of a copy are held to the scope.
    let inside = scope.path().join("in.txt");
    fs::write(&inside, "i").unwrap();
    for command in ["fs.rename", "fs.copy"] {
        denied(
            &app,
            command,
            json!({ "from": inside.to_string_lossy(), "to": secret.to_string_lossy() }),
        )
        .await;
        denied(&app, command, json!({ "from": secret.to_string_lossy(), "to": scope.path().join("x").to_string_lossy() })).await;
    }
    assert!(inside.exists() && fs::read(&secret).unwrap() == b"s");
}

#[tokio::test]
async fn a_link_inside_the_scope_does_not_lead_out_of_it_and_removing_it_removes_the_link() {
    let outside = tempfile::tempdir().unwrap();
    let scope = tempfile::tempdir().unwrap();
    let target = outside.path().join("secret.txt");
    fs::write(&target, "s").unwrap();
    let inside = scope.path().join("inside.txt");
    fs::write(&inside, "i").unwrap();
    let out = scope.path().join("out");
    let near = scope.path().join("near");
    if !link(&target, &out) || !link(&inside, &near) {
        eprintln!("skipped: this account cannot create symbolic links");
        return;
    }
    let app = fixture_in(scope.path(), Decision::Allow).await;

    denied(&app, "fs.readFile", at(&out)).await;
    denied(&app, "fs.stat", at(&out)).await;
    assert_eq!(
        read(&app, &near).await.unwrap(),
        b"i",
        "a link to a file inside the scope is read through"
    );
    let stat = app.call("fs.lstat", at(&near)).await.unwrap();
    assert_eq!(stat["kind"], "symlink", "lstat looks at the link");
    let listing = app.call("fs.readDir", at(scope.path())).await.unwrap();
    let kinds: Vec<(&str, &str)> = listing
        .as_array()
        .unwrap()
        .iter()
        .map(|e| (e["name"].as_str().unwrap(), e["kind"].as_str().unwrap()))
        .collect();
    assert!(kinds.contains(&("out", "symlink")), "{kinds:?}");

    app.call("fs.remove", at(&out)).await.unwrap();
    assert!(fs::symlink_metadata(&out).is_err(), "the link is gone");
    assert_eq!(fs::read(&target).unwrap(), b"s", "what it led to is not");
    app.call("fs.remove", at(&near)).await.unwrap();
    assert!(inside.exists());
}

#[tokio::test]
async fn read_and_write_are_two_rights() {
    let readable = tempfile::tempdir().unwrap();
    let writable = tempfile::tempdir().unwrap();
    fs::write(readable.path().join("r.txt"), "r").unwrap();
    let app = fixture(
        &[(scope_of(readable.path()), Decision::Allow)],
        &[(scope_of(writable.path()), Decision::Allow)],
    )
    .await;
    assert_eq!(
        read(&app, &readable.path().join("r.txt")).await.unwrap(),
        b"r"
    );
    denied(&app, "fs.readFile", at(&writable.path().join("w.txt"))).await;
    assert_eq!(
        write(&app, &readable.path().join("x.txt"), b"x")
            .await
            .unwrap_err()
            .code,
        ErrorCode::PermissionDenied
    );
    write(&app, &writable.path().join("w.txt"), b"w")
        .await
        .unwrap();
    // copy: read here, write there.
    app.call(
        "fs.copy",
        json!({ "from": readable.path().join("r.txt").to_string_lossy(), "to": writable.path().join("c.txt").to_string_lossy() }),
    )
    .await
    .unwrap();
    assert_eq!(fs::read(writable.path().join("c.txt")).unwrap(), b"r");
    // rename changes both ends: both must be writable.
    for (from, to) in [
        (
            writable.path().join("c.txt"),
            readable.path().join("moved.txt"),
        ),
        (
            readable.path().join("r.txt"),
            writable.path().join("moved.txt"),
        ),
    ] {
        denied(
            &app,
            "fs.rename",
            json!({ "from": from.to_string_lossy(), "to": to.to_string_lossy() }),
        )
        .await;
    }
    assert!(readable.path().join("r.txt").exists() && writable.path().join("c.txt").exists());
}

#[tokio::test]
async fn a_path_the_user_picked_is_real_whatever_was_decided_about_the_scope() {
    let scope = tempfile::tempdir().unwrap();
    let picked = tempfile::tempdir().unwrap();
    fs::write(picked.path().join("picked.txt"), "p").unwrap();
    let app = fixture_in(scope.path(), Decision::Deny).await;
    let file = picked.path().join("picked.txt");
    denied(&app, "fs.readFile", at(&file)).await;
    app.session().grants().grant_read(&file).unwrap();
    assert_eq!(read(&app, &file).await.unwrap(), b"p");
    denied(&app, "fs.writeFile", at(&file)).await;
}

#[tokio::test]
async fn a_denied_scope_gives_the_refusal_of_a_scope_that_was_never_listed() {
    let scope = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    fs::write(scope.path().join("a.txt"), "a").unwrap();
    let app = fixture_in(scope.path(), Decision::Deny).await;
    let by_user = app
        .call_reply("fs.readFile", at(&scope.path().join("a.txt")), None)
        .await
        .unwrap_err();
    let by_manifest = app
        .call_reply("fs.readFile", at(&elsewhere.path().join("a.txt")), None)
        .await
        .unwrap_err();
    assert_eq!(
        by_user, by_manifest,
        "the application cannot tell whose decision it was"
    );
}

#[tokio::test]
async fn a_stand_in_is_a_folder_that_starts_empty_keeps_what_is_written_and_touches_nothing_real() {
    let scope = tempfile::tempdir().unwrap();
    fs::write(scope.path().join("real.txt"), "real").unwrap();
    fs::create_dir(scope.path().join("real-folder")).unwrap();
    let app = fixture_in(scope.path(), Decision::Substitute).await;
    let root = scope.path();

    // The scope is a folder, empty; what is really there is not seen.
    let stat = app.call("fs.stat", at(root)).await.unwrap();
    assert_eq!(stat["kind"], "dir");
    assert_eq!(app.call("fs.readDir", at(root)).await.unwrap(), json!([]));
    assert_eq!(
        read(&app, &root.join("real.txt")).await.unwrap_err().code,
        ErrorCode::NotFound,
        "an empty folder has no such file"
    );
    assert_eq!(
        app.call("fs.exists", at(&root.join("real.txt")))
            .await
            .unwrap(),
        json!(false)
    );

    // Writing succeeds and is found again; the real folder is as it was.
    write(&app, &root.join("note.txt"), b"mine").await.unwrap();
    assert_eq!(read(&app, &root.join("note.txt")).await.unwrap(), b"mine");
    app.call(
        "fs.mkdir",
        json!({ "path": root.join("a").join("b").to_string_lossy(), "recursive": true }),
    )
    .await
    .unwrap();
    write(&app, &root.join("a").join("b").join("deep.txt"), b"deep")
        .await
        .unwrap();
    let listing = app.call("fs.readDir", at(root)).await.unwrap();
    let names: Vec<&str> = listing
        .as_array()
        .unwrap()
        .iter()
        .map(|e| e["name"].as_str().unwrap())
        .collect();
    assert_eq!(names, ["a", "note.txt"]);
    assert_eq!(
        listing[1]["path"],
        json!(root.join("note.txt").to_string_lossy()),
        "the paths are the application's own, no sign of the stand-in"
    );
    let mut really: Vec<String> = fs::read_dir(root)
        .unwrap()
        .map(|e| e.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    really.sort();
    assert_eq!(
        really,
        ["real-folder", "real.txt"],
        "nothing reached the real folder"
    );

    // The same commands do the same things there.
    app.call("fs.rename", json!({ "from": root.join("note.txt").to_string_lossy(), "to": root.join("renamed.txt").to_string_lossy() }))
        .await
        .unwrap();
    app.call("fs.copy", json!({ "from": root.join("renamed.txt").to_string_lossy(), "to": root.join("copied.txt").to_string_lossy() }))
        .await
        .unwrap();
    assert_eq!(read(&app, &root.join("copied.txt")).await.unwrap(), b"mine");
    let error = app
        .call("fs.remove", at(&root.join("a")))
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::DirectoryNotEmpty);
    app.call(
        "fs.remove",
        json!({ "path": root.join("a").to_string_lossy(), "recursive": true }),
    )
    .await
    .unwrap();
    assert_eq!(
        app.call("fs.exists", at(&root.join("a"))).await.unwrap(),
        json!(false)
    );

    // A path outside the scope is the refusal it always was.
    let elsewhere = tempfile::tempdir().unwrap();
    denied(&app, "fs.readFile", at(&elsewhere.path().join("x"))).await;
}

#[tokio::test]
async fn what_a_stand_in_copies_in_or_out_is_real_on_the_side_that_is_real() {
    let real = tempfile::tempdir().unwrap();
    let made_up = tempfile::tempdir().unwrap();
    fs::write(real.path().join("r.txt"), "from the real folder").unwrap();
    let app = fixture(
        &[
            (scope_of(real.path()), Decision::Allow),
            (scope_of(made_up.path()), Decision::Substitute),
        ],
        &[
            (scope_of(real.path()), Decision::Allow),
            (scope_of(made_up.path()), Decision::Substitute),
        ],
    )
    .await;
    // Real into the stand-in: kept there.
    app.call("fs.copy", json!({ "from": real.path().join("r.txt").to_string_lossy(), "to": made_up.path().join("in.txt").to_string_lossy() }))
        .await
        .unwrap();
    assert_eq!(
        read(&app, &made_up.path().join("in.txt")).await.unwrap(),
        b"from the real folder"
    );
    assert!(
        !made_up.path().join("in.txt").exists(),
        "not on the real disk at that place"
    );
    // The stand-in out into the real folder: a real file.
    write(&app, &made_up.path().join("made.txt"), b"made up")
        .await
        .unwrap();
    app.call("fs.rename", json!({ "from": made_up.path().join("made.txt").to_string_lossy(), "to": real.path().join("moved.txt").to_string_lossy() }))
        .await
        .unwrap();
    assert_eq!(fs::read(real.path().join("moved.txt")).unwrap(), b"made up");
    assert_eq!(
        read(&app, &made_up.path().join("made.txt"))
            .await
            .unwrap_err()
            .code,
        ErrorCode::NotFound
    );
}

#[tokio::test]
async fn the_answers_use_the_spelling_of_the_path_the_application_used() {
    let scope = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let alias = outside.path().join("alias");
    if !link_to_folder(scope.path(), &alias) {
        eprintln!("skipped: this account cannot create symbolic links");
        return;
    }
    fs::write(scope.path().join("a.txt"), "a").unwrap();
    // The scope is the real folder; the application reaches it by another name.
    let app = fixture_in(scope.path(), Decision::Allow).await;
    let listing = app.call("fs.readDir", at(&alias)).await.unwrap();
    assert_eq!(
        listing[0]["path"],
        json!(alias.join("a.txt").to_string_lossy()),
        "the entry is named under the name the application used"
    );
    assert_eq!(read(&app, &alias.join("a.txt")).await.unwrap(), b"a");
}

#[cfg(any(unix, windows))]
fn link_to_folder(target: &Path, at: &Path) -> bool {
    #[cfg(unix)]
    let made = std::os::unix::fs::symlink(target, at);
    #[cfg(windows)]
    let made = std::os::windows::fs::symlink_dir(target, at);
    made.is_ok()
}
