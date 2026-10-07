// SPDX-License-Identifier: MIT OR Apache-2.0
//! What `fs` does to the disk when the user allowed everything the manifest asks for.
mod handles;
mod rights;

use std::fs;

use alef_core::{security::consent::Decision, ErrorCode};
use bytes::Bytes;
use serde_json::{json, Value};

use crate::{at, fixture_in, read, write};

fn names(listing: &Value) -> Vec<String> {
    listing
        .as_array()
        .unwrap()
        .iter()
        .map(|entry| entry["name"].as_str().unwrap().to_owned())
        .collect()
}

#[tokio::test]
async fn files_are_written_appended_read_and_described() {
    let directory = tempfile::tempdir().unwrap();
    let app = fixture_in(directory.path(), Decision::Allow).await;
    let file = directory.path().join("note.txt");

    write(&app, &file, "héllo".as_bytes()).await.unwrap();
    assert_eq!(read(&app, &file).await.unwrap(), "héllo".as_bytes());
    app.call_reply(
        "fs.writeFile",
        json!({ "path": file.to_string_lossy(), "append": true }),
        Some(Bytes::from_static(b" world")),
    )
    .await
    .unwrap();
    assert_eq!(
        read(&app, &file).await.unwrap(),
        "héllo world".as_bytes(),
        "appended after what was there"
    );
    write(&app, &file, b"short").await.unwrap();
    assert_eq!(
        read(&app, &file).await.unwrap(),
        b"short",
        "written over, not into"
    );

    let missing = directory.path().join("never-was.txt");
    let error = app
        .call_reply(
            "fs.writeFile",
            json!({ "path": missing.to_string_lossy(), "create": false }),
            Some(Bytes::from_static(b"x")),
        )
        .await
        .unwrap_err();
    assert_eq!(
        error.code,
        ErrorCode::NotFound,
        "create: false creates nothing"
    );
    assert!(!missing.exists());

    let stat = app.call("fs.stat", at(&file)).await.unwrap();
    assert_eq!(stat["kind"], "file");
    assert_eq!(stat["size"], 5);
    assert!(
        stat["modified"].as_f64().unwrap() > 1.0e12,
        "milliseconds since 1970"
    );
    assert_eq!(stat["readonly"], false);
    let folder = app.call("fs.stat", at(directory.path())).await.unwrap();
    assert_eq!(
        (folder["kind"].as_str(), folder["size"].as_u64()),
        (Some("dir"), Some(0))
    );
    assert_eq!(app.call("fs.exists", at(&file)).await.unwrap(), json!(true));
    assert_eq!(
        app.call("fs.exists", at(&missing)).await.unwrap(),
        json!(false)
    );
}

#[tokio::test]
async fn folders_are_made_listed_renamed_copied_and_removed() {
    let directory = tempfile::tempdir().unwrap();
    let app = fixture_in(directory.path(), Decision::Allow).await;
    let root = directory.path();
    let deep = root.join("a").join("b");

    let error = app
        .call("fs.mkdir", json!({ "path": deep.to_string_lossy() }))
        .await
        .unwrap_err();
    assert_eq!(
        error.code,
        ErrorCode::NotFound,
        "no parents without recursive"
    );
    app.call(
        "fs.mkdir",
        json!({ "path": deep.to_string_lossy(), "recursive": true }),
    )
    .await
    .unwrap();
    app.call(
        "fs.mkdir",
        json!({ "path": deep.to_string_lossy(), "recursive": true }),
    )
    .await
    .unwrap();
    let error = app
        .call("fs.mkdir", json!({ "path": deep.to_string_lossy() }))
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::AlreadyExists);

    write(&app, &deep.join("two.txt"), b"22").await.unwrap();
    write(&app, &deep.join("one.txt"), b"1").await.unwrap();
    write(&app, &deep.join("B.txt"), b"B").await.unwrap();
    write(&app, &deep.join("Z.txt"), b"Z").await.unwrap();
    fs::create_dir(deep.join("sub")).unwrap();
    let listing = app.call("fs.readDir", at(&deep)).await.unwrap();
    assert_eq!(
        names(&listing),
        ["B.txt", "Z.txt", "one.txt", "sub", "two.txt"],
        "sorted by name, as bytes: not as this disk likes it"
    );
    let first = &listing[2];
    assert_eq!(first["kind"], "file");
    assert_eq!(first["size"], 1);
    assert_eq!(
        first["path"],
        json!(deep.join("one.txt").to_string_lossy()),
        "paths are the application's own"
    );
    assert_eq!(listing[3]["kind"], "dir");

    app.call(
        "fs.rename",
        json!({ "from": deep.join("one.txt").to_string_lossy(), "to": deep.join("uno.txt").to_string_lossy() }),
    )
    .await
    .unwrap();
    assert!(!deep.join("one.txt").exists() && deep.join("uno.txt").exists());

    app.call(
        "fs.copy",
        json!({ "from": deep.join("uno.txt").to_string_lossy(), "to": root.join("copy.txt").to_string_lossy() }),
    )
    .await
    .unwrap();
    assert_eq!(read(&app, &root.join("copy.txt")).await.unwrap(), b"1");
    app.call(
        "fs.copy",
        json!({ "from": root.join("a").to_string_lossy(), "to": root.join("a2").to_string_lossy() }),
    )
    .await
    .unwrap();
    assert_eq!(
        read(&app, &root.join("a2").join("b").join("two.txt"))
            .await
            .unwrap(),
        b"22"
    );
    let error = app
        .call(
            "fs.copy",
            json!({ "from": root.join("a").to_string_lossy(), "to": root.join("a2").to_string_lossy() }),
        )
        .await
        .unwrap_err();
    assert_eq!(
        error.code,
        ErrorCode::AlreadyExists,
        "a folder is not merged into another"
    );
    let error = app
        .call(
            "fs.copy",
            json!({ "from": root.join("a").to_string_lossy(), "to": deep.join("inside").to_string_lossy() }),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidArgument, "not into itself");
    assert!(error.message.contains("itself"), "{}", error.message);

    let error = app
        .call(
            "fs.remove",
            json!({ "path": root.join("a").to_string_lossy() }),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::DirectoryNotEmpty);
    app.call(
        "fs.remove",
        json!({ "path": root.join("a").to_string_lossy(), "recursive": true }),
    )
    .await
    .unwrap();
    assert!(!root.join("a").exists());
    app.call(
        "fs.remove",
        json!({ "path": root.join("copy.txt").to_string_lossy() }),
    )
    .await
    .unwrap();
    let error = app
        .call(
            "fs.remove",
            json!({ "path": root.join("copy.txt").to_string_lossy() }),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::NotFound);
}

#[tokio::test]
async fn failures_have_the_code_of_their_cause_and_words_that_tell_nothing_of_the_machine() {
    let directory = tempfile::tempdir().unwrap();
    let app = fixture_in(directory.path(), Decision::Allow).await;
    let file = directory.path().join("a-file");
    fs::write(&file, "x").unwrap();
    let missing = directory.path().join("nothing-here");

    let cases = [
        (
            app.call("fs.readDir", at(&file)).await.unwrap_err(),
            ErrorCode::NotADirectory,
        ),
        (
            read(&app, directory.path()).await.unwrap_err(),
            ErrorCode::IsADirectory,
        ),
        (
            write(&app, directory.path(), b"x").await.unwrap_err(),
            ErrorCode::IsADirectory,
        ),
        (read(&app, &missing).await.unwrap_err(), ErrorCode::NotFound),
        (
            app.call("fs.stat", at(&missing)).await.unwrap_err(),
            ErrorCode::NotFound,
        ),
        (
            app.call("fs.mkdir", at(&file)).await.unwrap_err(),
            ErrorCode::AlreadyExists,
        ),
    ];
    for (error, code) in cases {
        assert_eq!(error.code, code, "{error}");
        assert!(
            error
                .message
                .chars()
                .all(|c| c.is_ascii_lowercase() || c == ' '),
            "plain words, no path and no quote of the system: {}",
            error.message
        );
        assert_eq!(error.details, None);
    }
}

#[tokio::test]
async fn a_whole_file_is_for_small_files_and_the_rest_goes_through_a_handle() {
    let directory = tempfile::tempdir().unwrap();
    let app = fixture_in(directory.path(), Decision::Allow).await;
    let big = directory.path().join("big.bin");
    let size = 64 * 1024 * 1024 + 1;
    fs::write(&big, vec![0u8; size]).unwrap();
    let error = read(&app, &big).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidArgument);
    assert!(error.message.contains("fs.open"), "{}", error.message);

    let error = write(
        &app,
        &directory.path().join("too-big.bin"),
        &vec![1u8; size],
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidArgument);
    assert!(
        !directory.path().join("too-big.bin").exists(),
        "nothing was started"
    );
}

#[tokio::test]
async fn a_scratch_file_or_folder_is_the_documents_to_use_and_nobody_elses() {
    let directory = tempfile::tempdir().unwrap();
    let app = fixture_in(directory.path(), Decision::Allow).await;
    let file = app.call("fs.tempFile", Value::Null).await.unwrap();
    let file = std::path::PathBuf::from(file.as_str().unwrap());
    assert!(file.is_file(), "{file:?}");
    write(&app, &file, b"scratch").await.unwrap();
    assert_eq!(read(&app, &file).await.unwrap(), b"scratch");

    let folder = app.call("fs.tempDir", Value::Null).await.unwrap();
    let folder = std::path::PathBuf::from(folder.as_str().unwrap());
    assert!(folder.is_dir());
    write(&app, &folder.join("inner.txt"), b"in").await.unwrap();
    let listing = app.call("fs.readDir", at(&folder)).await.unwrap();
    assert_eq!(names(&listing), ["inner.txt"]);

    let other = app.open_window(2).await;
    let error = app.call_as(&other, "fs.stat", at(&file)).await.unwrap_err();
    assert_eq!(
        error.code,
        ErrorCode::PermissionDenied,
        "the grant belongs to the document that asked"
    );
    let _ = fs::remove_file(&file);
    let _ = fs::remove_dir_all(&folder);
}
