// SPDX-License-Identifier: MIT OR Apache-2.0
//! `http.download`: the file, the progress, the scope of `fs.write`, the stand-in, the redirects.
use super::*;

fn write_scope(directory: &Path) -> String {
    format!("{}/**", directory.display().to_string().replace('\\', "/"))
}

/// The progress frames of a download until it ends: `Ok(frames)` or the error it ended with.
async fn follow_download(app: &Fixture, started: &Value) -> Result<Vec<Value>, AlefError> {
    let id = alef_core::ids::StreamId(started["stream"].as_u64().unwrap());
    let session = app.session();
    let mut reader = session.streams().reader(id).expect("an outgoing stream");
    let mut frames = Vec::new();
    loop {
        match tokio::time::timeout(Duration::from_secs(30), reader.next_frame())
            .await
            .expect("the download stalled")
        {
            Some(Frame::Json(value)) => {
                session.streams().ack(id, value.to_string().len()).unwrap();
                frames.push(value);
            }
            Some(Frame::End) | None => return Ok(frames),
            Some(Frame::Error(error)) => return Err(error),
            Some(Frame::Binary(_)) => panic!("a download carries progress, not bytes"),
        }
    }
}

#[tokio::test]
async fn a_download_fills_a_file_with_progress_and_leaves_none_when_it_fails() {
    let server = Server::start().await;
    let directory = tempfile::tempdir().unwrap();
    let app = Fixture::new(
        Some(&manifest(
            &[server.scope()],
            &[write_scope(directory.path())],
        )),
        &[],
    )
    .await;
    let target = directory.path().join("big.bin");
    let started = app
        .call(
            "http.download",
            json!({ "url": server.url("/big"), "path": target.to_string_lossy() }),
        )
        .await
        .unwrap();
    let frames = follow_download(&app, &started).await.unwrap();
    let last = frames.last().expect("a frame");
    assert_eq!(
        (
            last["done"].clone(),
            last["received"].as_u64(),
            last["total"].as_u64()
        ),
        (json!(true), Some(BIG as u64), Some(BIG as u64))
    );
    assert!(
        frames.len() >= 10,
        "progress came along the way: {}",
        frames.len()
    );
    assert!(frames
        .windows(2)
        .all(|pair| pair[0]["received"].as_u64() <= pair[1]["received"].as_u64()));
    let written = std::fs::read(&target).unwrap();
    assert_eq!(written.len(), BIG);
    assert!(
        written
            .iter()
            .enumerate()
            .all(|(at, byte)| *byte == big_byte(at)),
        "the file came whole"
    );

    // An answer of failure is an error before any file is made.
    let missing = directory.path().join("missing.bin");
    let refused = app
        .call(
            "http.download",
            json!({ "url": server.url("/notfound"), "path": missing.to_string_lossy() }),
        )
        .await
        .unwrap_err();
    assert_eq!(refused.code, ErrorCode::Network);
    assert!(!missing.exists());

    // A connection that breaks on the way leaves no half of a file.
    let cut = directory.path().join("cut.bin");
    let started = app
        .call(
            "http.download",
            json!({ "url": server.url("/cut"), "path": cut.to_string_lossy() }),
        )
        .await
        .unwrap();
    let ended = follow_download(&app, &started).await.unwrap_err();
    assert_eq!(ended.code, ErrorCode::Network);
    tokio::time::sleep(Duration::from_millis(200)).await;
    assert!(!cut.exists(), "the half of a file was left");

    // A page that stops listening stops the download and the file with it.
    let stopped = directory.path().join("stopped.bin");
    let started = app
        .call(
            "http.download",
            json!({ "url": server.url("/big"), "path": stopped.to_string_lossy() }),
        )
        .await
        .unwrap();
    let id = alef_core::ids::StreamId(started["stream"].as_u64().unwrap());
    app.session().streams().close(id).unwrap();
    for _ in 0..50 {
        if !stopped.exists() {
            break;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(
        !stopped.exists(),
        "a file of a download nobody listens to was left"
    );
}

#[tokio::test]
async fn a_download_goes_only_where_fs_write_lets_it_and_the_stand_in_takes_what_the_user_chose() {
    let server = Server::start().await;
    let allowed = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    let app = Fixture::new(
        Some(&manifest(&[server.scope()], &[write_scope(allowed.path())])),
        &[],
    )
    .await;
    let before = server.requests().len();
    let outside = elsewhere.path().join("x.bin");
    let denied = app
        .call(
            "http.download",
            json!({ "url": server.url("/hello"), "path": outside.to_string_lossy() }),
        )
        .await
        .unwrap_err();
    assert_eq!(denied.code, ErrorCode::PermissionDenied);
    assert_eq!(denied.details, Some(json!({ "permission": "fs.write" })));
    assert_eq!(
        server.requests().len(),
        before,
        "nothing was asked of the server"
    );
    assert!(!outside.exists());

    let scope = write_scope(allowed.path());
    let mut consent = Consent::undecided();
    consent.set(Right::scoped("fs.write", &scope), Decision::Substitute);
    consent.set(Right::scoped("net.http", &server.scope()), Decision::Allow);
    let app = Fixture::new(Some(&manifest(&[server.scope()], &[scope])), &[])
        .await
        .with_consent(consent);
    let target = allowed.path().join("kept.txt");
    let started = app
        .call(
            "http.download",
            json!({ "url": server.url("/hello"), "path": target.to_string_lossy() }),
        )
        .await
        .unwrap();
    follow_download(&app, &started).await.unwrap();
    assert!(!target.exists(), "the real folder stays as it was");
}

#[tokio::test]
async fn a_download_is_the_dead_network_when_the_user_substituted_it() {
    let server = Server::start().await;
    let directory = tempfile::tempdir().unwrap();
    let app = Fixture::new(
        Some(&manifest(
            &[server.scope()],
            &[write_scope(directory.path())],
        )),
        &[],
    )
    .await
    .with_consent(substituting(&server.scope()));
    let target = directory.path().join("never.bin");
    let error = app
        .call("http.download", json!({ "url": server.url("/hello"), "path": target.to_string_lossy(), "timeoutMs": 200 }))
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::Timeout);
    assert!(!target.exists() && server.requests().is_empty());
}

#[tokio::test]
async fn a_download_follows_redirects_and_holds_every_hop_against_the_scope() {
    let server = Server::start().await;
    let outside = Server::start().await;
    let directory = tempfile::tempdir().unwrap();
    let app = Fixture::new(
        Some(&manifest(
            &[server.scope()],
            &[write_scope(directory.path())],
        )),
        &[],
    )
    .await;
    let target = directory.path().join("hello.txt");
    let started = app
        .call(
            "http.download",
            json!({ "url": server.url("/redirect/302?to=/hello"), "path": target.to_string_lossy() }),
        )
        .await
        .unwrap();
    follow_download(&app, &started).await.unwrap();
    assert_eq!(std::fs::read(&target).unwrap(), b"hello");

    let away = directory.path().join("away.txt");
    let moved = outside.url("/hello");
    let denied = app
        .call(
            "http.download",
            json!({ "url": server.url(&format!("/redirect/302?to={moved}")), "path": away.to_string_lossy() }),
        )
        .await
        .unwrap_err();
    assert_eq!(denied.code, ErrorCode::PermissionDenied);
    assert!(outside.requests().is_empty() && !away.exists());
}
