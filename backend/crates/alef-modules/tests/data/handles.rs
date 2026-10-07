// SPDX-License-Identifier: MIT OR Apache-2.0
//! Open files, the streams of files and of folders, and the watch, through the registry.
use std::{path::Path, time::Duration};

use alef_core::{
    ids::StreamId, protocol::frame::Frame, registry::command::Reply, security::consent::Decision,
    session::StreamReader, ErrorCode,
};
use bytes::Bytes;
use serde_json::{json, Value};

use super::{at, fixture, fixture_in, read, scope_of, write};
use crate::common::Fixture;

async fn open(app: &Fixture, path: &Path, flags: Value) -> u64 {
    let mut args = flags;
    args["path"] = json!(path.to_string_lossy());
    app.call("fs.open", args).await.unwrap()["handle"]
        .as_u64()
        .unwrap()
}

async fn call_bytes(app: &Fixture, command: &str, args: Value, body: Option<&[u8]>) -> Vec<u8> {
    match app
        .call_reply(command, args, body.map(Bytes::copy_from_slice))
        .await
        .unwrap()
    {
        Reply::Bytes(bytes) => bytes.to_vec(),
        other => panic!("expected bytes, got {other:?}"),
    }
}

fn stream_of(reply: &Value) -> StreamId {
    StreamId(reply["stream"].as_u64().unwrap())
}

/// Everything a stream of binary frames carries, acknowledging as a page does.
async fn drain(app: &Fixture, id: StreamId) -> Vec<u8> {
    let session = app.session();
    let mut reader = session.streams().reader(id).expect("an outgoing stream");
    let mut data = Vec::new();
    loop {
        match tokio::time::timeout(Duration::from_secs(20), reader.next_frame())
            .await
            .expect("the stream stalled")
        {
            Some(Frame::Binary(bytes)) => {
                session.streams().ack(id, bytes.len()).unwrap();
                data.extend_from_slice(&bytes);
            }
            Some(Frame::End) | None => return data,
            Some(Frame::Error(error)) => panic!("the stream failed: {error}"),
            Some(Frame::Json(value)) => panic!("unexpected {value}"),
        }
    }
}

#[tokio::test]
async fn a_handle_reads_and_writes_at_its_position_or_at_one_it_is_given() {
    let directory = tempfile::tempdir().unwrap();
    let app = fixture_in(directory.path(), Decision::Allow).await;
    let file = directory.path().join("h.bin");
    std::fs::write(&file, b"0123456789").unwrap();

    let handle = open(&app, &file, json!({ "read": true, "write": true })).await;
    let piece = |length: u64, position: Option<u64>| {
        let mut args = json!({ "handle": handle, "length": length });
        if let Some(position) = position {
            args["position"] = json!(position);
        }
        args
    };
    assert_eq!(
        call_bytes(&app, "fs.read", piece(4, None), None).await,
        b"0123"
    );
    assert_eq!(
        call_bytes(&app, "fs.read", piece(3, None), None).await,
        b"456",
        "it goes on where it was"
    );
    assert_eq!(
        call_bytes(&app, "fs.read", piece(2, Some(8)), None).await,
        b"89",
        "a position does not move it"
    );
    assert_eq!(
        call_bytes(&app, "fs.read", piece(9, None), None).await,
        b"789"
    );
    assert!(
        call_bytes(&app, "fs.read", piece(9, None), None)
            .await
            .is_empty(),
        "the end of the file is an empty piece"
    );

    let written = app
        .call_reply(
            "fs.write",
            json!({ "handle": handle, "position": 2 }),
            Some(Bytes::from_static(b"AB")),
        )
        .await
        .unwrap();
    assert!(matches!(written, Reply::Json(ref value) if value["written"] == 2));
    assert_eq!(read(&app, &file).await.unwrap(), b"01AB456789");
    assert!(
        call_bytes(&app, "fs.read", piece(3, None), None)
            .await
            .is_empty(),
        "a write at a position does not move the handle"
    );
    let error = app
        .call_reply(
            "fs.read",
            json!({ "handle": handle, "length": 16 * 1024 * 1024 + 1 }),
            None,
        )
        .await
        .unwrap_err();
    assert_eq!(
        error.code,
        ErrorCode::InvalidArgument,
        "a stream is for more than 16 MiB"
    );
    let stat = app
        .call("fs.fstat", json!({ "handle": handle }))
        .await
        .unwrap();
    assert_eq!(
        (stat["kind"].as_str(), stat["size"].as_u64()),
        (Some("file"), Some(10))
    );
    app.call("fs.truncate", json!({ "handle": handle, "length": 4 }))
        .await
        .unwrap();
    app.call("fs.sync", json!({ "handle": handle }))
        .await
        .unwrap();
    assert_eq!(read(&app, &file).await.unwrap(), b"01AB");
    app.call("fs.close", json!({ "handle": handle }))
        .await
        .unwrap();
    assert_eq!(
        app.call("fs.fstat", json!({ "handle": handle }))
            .await
            .unwrap_err()
            .code,
        ErrorCode::NotFound,
        "a closed handle is gone"
    );
}

#[tokio::test]
async fn a_handle_does_only_what_it_was_opened_for_and_appends_at_the_end() {
    let directory = tempfile::tempdir().unwrap();
    let app = fixture_in(directory.path(), Decision::Allow).await;
    let file = directory.path().join("a.txt");
    std::fs::write(&file, "abc").unwrap();

    let reading = open(&app, &file, json!({})).await;
    let error = app
        .call_reply(
            "fs.write",
            json!({ "handle": reading }),
            Some(Bytes::from_static(b"x")),
        )
        .await
        .unwrap_err();
    assert_eq!(
        error.code,
        ErrorCode::PermissionDenied,
        "opened for reading only"
    );
    let writing = open(&app, &file, json!({ "append": true })).await;
    app.call_reply(
        "fs.write",
        json!({ "handle": writing }),
        Some(Bytes::from_static(b"def")),
    )
    .await
    .unwrap();
    app.call_reply(
        "fs.write",
        json!({ "handle": writing, "position": 0 }),
        Some(Bytes::from_static(b"ghi")),
    )
    .await
    .unwrap();
    assert_eq!(
        read(&app, &file).await.unwrap(),
        b"abcdefghi",
        "append goes to the end whatever the position"
    );
    let error = app
        .call_reply("fs.read", json!({ "handle": writing, "length": 1 }), None)
        .await
        .unwrap_err();
    assert_eq!(
        error.code,
        ErrorCode::PermissionDenied,
        "opened for writing only"
    );
    let error = app
        .call("fs.readStream", json!({ "handle": writing }))
        .await
        .unwrap_err();
    assert_eq!(
        error.code,
        ErrorCode::PermissionDenied,
        "no stream out of a handle opened for writing only"
    );
    let error = app
        .call("fs.writeStream", json!({ "handle": reading }))
        .await
        .unwrap_err();
    assert_eq!(
        error.code,
        ErrorCode::PermissionDenied,
        "no stream into a handle opened for reading only"
    );

    let flags = |extra: Value| {
        let mut args = extra;
        args["path"] = json!(file.to_string_lossy());
        args
    };
    for bad in [
        json!({ "truncate": true }),
        json!({ "create": true }),
        json!({ "append": true, "truncate": true }),
        json!({ "write": true, "append": true, "truncate": true }),
    ] {
        let error = app.call("fs.open", flags(bad.clone())).await.unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidArgument, "{bad}");
    }
    let emptied = open(&app, &file, json!({ "write": true, "truncate": true })).await;
    assert_eq!(
        read(&app, &file).await.unwrap(),
        b"",
        "truncate empties the file"
    );
    app.call("fs.close", json!({ "handle": emptied }))
        .await
        .unwrap();
    let missing = directory.path().join("missing.txt");
    let error = app
        .call("fs.open", json!({ "path": missing.to_string_lossy() }))
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::NotFound);
    let error = app
        .call(
            "fs.open",
            json!({ "path": missing.to_string_lossy(), "createNew": true, "write": true }),
        )
        .await;
    assert!(error.is_ok(), "createNew makes the file");
    let error = app
        .call(
            "fs.open",
            json!({ "path": missing.to_string_lossy(), "createNew": true, "write": true }),
        )
        .await
        .unwrap_err();
    assert_eq!(
        error.code,
        ErrorCode::AlreadyExists,
        "createNew does not take a file that is there"
    );
    let error = app.call("fs.open", at(directory.path())).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::IsADirectory);
    let outside = tempfile::tempdir().unwrap();
    let error = app
        .call("fs.open", at(&outside.path().join("x")))
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::PermissionDenied);
}

#[tokio::test]
async fn a_handle_belongs_to_its_document_and_goes_with_it() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = fixture_in(directory.path(), Decision::Allow).await;
    let file = directory.path().join("own.txt");
    std::fs::write(&file, "x").unwrap();
    let handle = open(&app, &file, json!({})).await;
    let other = app.open_window(2).await;
    let error = app
        .call_as(&other, "fs.fstat", json!({ "handle": handle }))
        .await
        .unwrap_err();
    assert_eq!(
        error.code,
        ErrorCode::NotFound,
        "another document has no such handle"
    );
    assert_eq!(app.session().resources().len(), 1);
    app.reload_document().await;
    assert_eq!(
        app.session().resources().len(),
        0,
        "the new document has none"
    );
    // The old handle is closed with its session: the file can go (Windows would refuse it otherwise).
    std::fs::remove_file(&file).unwrap();
}

#[tokio::test]
async fn a_file_passes_through_streams_in_both_directions_and_arrives_whole() {
    let directory = tempfile::tempdir().unwrap();
    let app = fixture_in(directory.path(), Decision::Allow).await;
    let source = directory.path().join("source.bin");
    let size = 3 * 1024 * 1024 + 12345;
    let pattern: Vec<u8> = (0..size)
        .map(|i| ((i * 31 + (i >> 8)) & 255) as u8)
        .collect();
    std::fs::write(&source, &pattern).unwrap();

    let from = open(&app, &source, json!({})).await;
    let started = app
        .call("fs.readStream", json!({ "handle": from }))
        .await
        .unwrap();
    let streamed = drain(&app, stream_of(&started)).await;
    assert_eq!(streamed.len(), pattern.len());
    assert!(streamed == pattern, "the file came whole and in order");

    let part = app
        .call(
            "fs.readStream",
            json!({ "handle": from, "position": 1000, "length": 5000 }),
        )
        .await
        .unwrap();
    assert_eq!(drain(&app, stream_of(&part)).await, &pattern[1000..6000]);

    let target = directory.path().join("target.bin");
    let to = open(
        &app,
        &target,
        json!({ "write": true, "create": true, "truncate": true }),
    )
    .await;
    let opened = app
        .call("fs.writeStream", json!({ "handle": to }))
        .await
        .unwrap();
    let id = stream_of(&opened);
    let writer = app
        .session()
        .streams()
        .incoming_writer(id)
        .expect("an incoming stream");
    for piece in pattern.chunks(256 * 1024) {
        writer.write(Bytes::copy_from_slice(piece)).await.unwrap();
    }
    writer.end();
    app.call("fs.settle", json!({ "handle": to }))
        .await
        .unwrap();
    assert!(
        std::fs::read(&target).unwrap() == pattern,
        "the copy is the file"
    );
    app.call("fs.close", json!({ "handle": to })).await.unwrap();
    app.call("fs.close", json!({ "handle": from }))
        .await
        .unwrap();
}

#[tokio::test]
async fn a_stream_into_a_file_may_start_at_a_position_and_goes_on_from_there() {
    let directory = tempfile::tempdir().unwrap();
    let app = fixture_in(directory.path(), Decision::Allow).await;
    let target = directory.path().join("at.bin");
    std::fs::write(&target, vec![b'.'; 20]).unwrap();
    let to = open(&app, &target, json!({ "write": true })).await;
    let opened = app
        .call("fs.writeStream", json!({ "handle": to, "position": 5 }))
        .await
        .unwrap();
    let writer = app
        .session()
        .streams()
        .incoming_writer(stream_of(&opened))
        .unwrap();
    writer.write(Bytes::from_static(b"abc")).await.unwrap();
    writer.write(Bytes::from_static(b"def")).await.unwrap();
    writer.end();
    app.call("fs.settle", json!({ "handle": to }))
        .await
        .unwrap();
    assert_eq!(std::fs::read(&target).unwrap(), b".....abcdef.........");
}

#[tokio::test]
async fn a_stream_that_is_cut_off_ends_the_work_and_settle_says_so() {
    let directory = tempfile::tempdir().unwrap();
    let app = fixture_in(directory.path(), Decision::Allow).await;
    let target = directory.path().join("cut.bin");
    let to = open(&app, &target, json!({ "write": true, "create": true })).await;
    let opened = app
        .call("fs.writeStream", json!({ "handle": to }))
        .await
        .unwrap();
    let id = stream_of(&opened);
    let writer = app.session().streams().incoming_writer(id).unwrap();
    writer.write(Bytes::from_static(b"half")).await.unwrap();
    app.session().streams().close(id).unwrap();
    let error = app
        .call("fs.settle", json!({ "handle": to }))
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::Closed, "what was cut off is told");
    let busy = app.call("fs.writeStream", json!({ "handle": to })).await;
    assert!(busy.is_ok(), "settled: a new stream may start");
    let first = app
        .call("fs.writeStream", json!({ "handle": to }))
        .await
        .unwrap_err();
    assert_eq!(first.code, ErrorCode::Busy, "one stream at a time");
}

#[tokio::test]
async fn a_big_folder_comes_in_batches_and_a_missing_one_is_told_at_once() {
    let directory = tempfile::tempdir().unwrap();
    let app = fixture_in(directory.path(), Decision::Allow).await;
    for index in 0..1203 {
        std::fs::write(directory.path().join(format!("f{index:04}.txt")), "x").unwrap();
    }
    let opened = app
        .call("fs.readDirStream", at(directory.path()))
        .await
        .unwrap();
    let session = app.session();
    let mut reader = session.streams().reader(stream_of(&opened)).unwrap();
    let mut names = Vec::new();
    let mut frames = 0;
    loop {
        match tokio::time::timeout(Duration::from_secs(20), reader.next_frame())
            .await
            .expect("the stream stalled")
        {
            Some(Frame::Json(batch)) => {
                frames += 1;
                for entry in batch.as_array().unwrap() {
                    names.push(entry["name"].as_str().unwrap().to_owned());
                    assert!(entry["path"]
                        .as_str()
                        .unwrap()
                        .ends_with(entry["name"].as_str().unwrap()));
                }
            }
            Some(Frame::End) | None => break,
            other => panic!("unexpected {other:?}"),
        }
    }
    assert_eq!(names.len(), 1203);
    assert!(frames >= 3, "the entries came in batches: {frames}");
    names.sort();
    names.dedup();
    assert_eq!(names.len(), 1203, "each entry once");

    let error = app
        .call("fs.readDirStream", at(&directory.path().join("nothing")))
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::NotFound);
    write(&app, &directory.path().join("file"), b"x")
        .await
        .unwrap();
    let error = app
        .call("fs.readDirStream", at(&directory.path().join("file")))
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::NotADirectory);
}

/// The kinds and paths of the events of a stream, until `until` says it has seen enough.
async fn events(
    app: &Fixture,
    reader: &mut StreamReader,
    until: impl Fn(&[(String, String)]) -> bool,
) -> Vec<(String, String)> {
    let session = app.session();
    let id = reader.id();
    let mut seen = Vec::new();
    while !until(&seen) {
        match tokio::time::timeout(Duration::from_secs(20), reader.next_frame())
            .await
            .expect("no event came")
        {
            Some(Frame::Json(event)) => {
                session.streams().ack(id, event.to_string().len()).unwrap();
                seen.push((
                    event["kind"].as_str().unwrap().to_owned(),
                    event["path"].as_str().unwrap().to_owned(),
                ));
            }
            other => panic!("unexpected {other:?}"),
        }
    }
    seen
}

#[tokio::test]
async fn a_watch_tells_what_happens_in_a_folder_in_the_names_of_the_application() {
    let directory = tempfile::tempdir().unwrap();
    let app = fixture_in(directory.path(), Decision::Allow).await;
    let watched = directory.path().to_path_buf();
    std::fs::create_dir(watched.join("sub")).unwrap();
    let opened = app
        .call(
            "fs.watch",
            json!({ "path": directory.path().to_string_lossy(), "recursive": true }),
        )
        .await
        .unwrap();
    let id = stream_of(&opened);
    let mut reader = app.session().streams().reader(id).unwrap();
    let file = watched.join("new.txt");
    std::fs::write(&file, "1").unwrap();
    let created = events(&app, &mut reader, |seen| {
        seen.iter()
            .any(|(kind, path)| kind == "create" && path.ends_with("new.txt"))
    })
    .await;
    assert!(
        created
            .iter()
            .all(|(_, path)| Path::new(path).starts_with(&watched)),
        "{created:?}"
    );
    std::fs::remove_file(&file).unwrap();
    let removed = events(&app, &mut reader, |seen| {
        seen.iter()
            .any(|(kind, path)| kind == "remove" && path.ends_with("new.txt"))
    })
    .await;
    assert!(!removed.is_empty());
    std::fs::write(watched.join("sub").join("deep.txt"), "d").unwrap();
    events(&app, &mut reader, |seen| {
        seen.iter().any(|(_, path)| path.ends_with("deep.txt"))
    })
    .await;
    app.session().streams().close(id).unwrap();

    let error = app
        .call(
            "fs.watch",
            json!({ "path": directory.path().join("missing").to_string_lossy() }),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::NotFound);
    let outside = tempfile::tempdir().unwrap();
    let error = app.call("fs.watch", at(outside.path())).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::PermissionDenied);
}

#[tokio::test]
async fn a_watch_of_a_stand_in_tells_of_the_writes_of_the_application_only() {
    let real = tempfile::tempdir().unwrap();
    let app = fixture_in(real.path(), Decision::Substitute).await;
    std::fs::write(real.path().join("real.txt"), "r").unwrap();
    let opened = app
        .call("fs.watch", json!({ "path": real.path().to_string_lossy() }))
        .await
        .unwrap();
    let id = stream_of(&opened);
    let mut reader = app.session().streams().reader(id).unwrap();
    let note = real.path().join("note.txt");
    write(&app, &note, b"n").await.unwrap();
    let seen = events(&app, &mut reader, |seen| {
        seen.iter().any(|(_, path)| path.ends_with("note.txt"))
    })
    .await;
    for (_, path) in &seen {
        assert!(
            Path::new(path).starts_with(real.path()),
            "paths are the application's own, not the stand-in's: {path}"
        );
    }
    app.session().streams().close(id).unwrap();
}

async fn refused(app: &Fixture, path: &Path, flags: Value) {
    let mut args = flags;
    args["path"] = json!(path.to_string_lossy());
    let error = app.call("fs.open", args).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::PermissionDenied, "{path:?}");
}

#[tokio::test]
async fn a_handle_needs_the_right_for_what_it_is_opened_for() {
    let readable = tempfile::tempdir().unwrap();
    let writable = tempfile::tempdir().unwrap();
    let (r, w) = (readable.path().join("r.txt"), writable.path().join("w.txt"));
    std::fs::write(&r, "r").unwrap();
    std::fs::write(&w, "w").unwrap();
    let app = fixture(
        &[(scope_of(readable.path()), Decision::Allow)],
        &[(scope_of(writable.path()), Decision::Allow)],
    )
    .await;
    open(&app, &r, json!({})).await;
    refused(&app, &r, json!({ "write": true })).await;
    open(&app, &w, json!({ "write": true })).await;
    refused(&app, &w, json!({})).await;
    refused(&app, &r, json!({ "read": true, "write": true })).await;
}

#[tokio::test]
async fn one_file_is_not_real_for_reading_and_a_stand_in_for_writing() {
    let both = tempfile::tempdir().unwrap();
    let file = both.path().join("b.txt");
    std::fs::write(&file, "b").unwrap();
    let scope = scope_of(both.path());
    let app = fixture(
        &[(scope.clone(), Decision::Allow)],
        &[(scope, Decision::Substitute)],
    )
    .await;
    refused(&app, &file, json!({ "read": true, "write": true })).await;
    open(&app, &file, json!({})).await;
    let stand_in = open(
        &app,
        &file,
        json!({ "write": true, "create": true, "truncate": true }),
    )
    .await;
    app.call("fs.close", json!({ "handle": stand_in }))
        .await
        .unwrap();
    assert_eq!(
        std::fs::read(&file).unwrap(),
        b"b",
        "the write went to the stand-in"
    );
}
