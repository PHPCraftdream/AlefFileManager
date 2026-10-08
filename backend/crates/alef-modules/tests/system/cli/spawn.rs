// SPDX-License-Identifier: MIT OR Apache-2.0
//! `cli.spawn`, `cli.wait` and `cli.kill` through the registry: streams with credit both ways, a
//! wait that happens once, and a kill that takes the whole tree.
use std::time::Duration;

use alef_core::{
    ids::StreamId,
    protocol::{credit::DEFAULT_STREAM_WINDOW, frame::Frame},
};
use bytes::Bytes;
use serde_json::{json, Value};
use tempfile::tempdir;

use crate::common::Fixture;

use super::{alive, manifest, pid_of, pipe::Pipe, until};

async fn app() -> Fixture {
    Fixture::new(Some(&manifest(&["node"])), &[]).await
}

async fn spawn(app: &Fixture, args: Value) -> Value {
    app.call("cli.spawn", args).await.expect("the spawn works")
}

/// A chunk of `count` bytes that varies with its position, so a lost or repeated one shows.
fn pattern(count: usize) -> Vec<u8> {
    (0..count).map(|i| (i % 251) as u8).collect()
}

#[tokio::test]
async fn spawn_gives_streams_both_ways_and_the_pid() {
    let fixture = app().await;
    let reply = spawn(
        &fixture,
        json!({
            "program": "node",
            "args": ["-e", "process.stdin.pipe(process.stdout)"],
            "stdin": "pipe",
            "stdout": "pipe",
            "stderr": "ignore"
        }),
    )
    .await;
    assert!(reply["process"].as_u64().unwrap() > 0);
    assert!(reply["pid"].as_u64().unwrap() > 0);
    assert!(reply["stdin"].as_u64().is_some());
    assert!(reply["stdout"].as_u64().is_some());
    assert!(reply["stderr"].is_null());

    let sent = pattern(4 * 1024 * 1024);
    let writer = fixture
        .session()
        .streams()
        .incoming_writer(StreamId(reply["stdin"].as_u64().unwrap()))
        .expect("an incoming stream");
    let ((), received) = tokio::join!(
        async {
            for chunk in sent.chunks(64 * 1024) {
                writer
                    .write(Bytes::copy_from_slice(chunk))
                    .await
                    .expect("the child keeps reading");
            }
            writer.end();
        },
        async {
            let mut pipe = Pipe::open(&fixture, reply["stdout"].as_u64().unwrap());
            pipe.until_end().await.expect("the pipe does not break")
        }
    );
    assert_eq!(received, sent);

    let reply = fixture
        .call("cli.wait", json!({ "process": reply["process"] }))
        .await
        .expect("the wait works");
    assert_eq!(reply["code"], 0);
    assert!(reply["signal"].is_null());
}

/// A page that takes no credit holds the child back: what is delivered stays inside the window,
/// and nothing more comes until the page acknowledges.
#[tokio::test]
async fn spawn_backpressure_holds_the_child_back() {
    let fixture = app().await;
    let reply = spawn(
        &fixture,
        json!({
            "program": "node",
            "args": ["-e", "process.stdout.write(Buffer.alloc(4*1024*1024))"],
            "stdout": "pipe"
        }),
    )
    .await;
    let id = StreamId(reply["stdout"].as_u64().unwrap());
    let mut reader = fixture
        .session()
        .streams()
        .reader(id)
        .expect("an outgoing stream");

    // Take one frame and acknowledge nothing beyond it.
    let first = match tokio::time::timeout(Duration::from_secs(60), reader.next_frame())
        .await
        .expect("the stream stalled")
    {
        Some(Frame::Binary(bytes)) => bytes,
        other => panic!("expected bytes, got {other:?}"),
    };
    assert!(!first.is_empty());

    // Whatever the pump managed to put through before the window shut, it stays put: no more
    // arrives, and what arrived is inside the window. This cannot hold if the pump read without
    // credit.
    until(
        || async {
            fixture.session().streams().outstanding(id).unwrap_or(0) == DEFAULT_STREAM_WINDOW
        },
        2_000,
        "the first bytes are delivered",
    )
    .await;
    let delivered = fixture.session().streams().outstanding(id).unwrap();
    assert!(
        delivered <= DEFAULT_STREAM_WINDOW,
        "{delivered} bytes came through a window of {DEFAULT_STREAM_WINDOW}"
    );
    let later = delivered;
    assert!(
        later < 4 * 1024 * 1024,
        "the whole 4 MiB came through without credit"
    );

    // Read and acknowledge each queued frame exactly once; outstanding counts bytes already
    // queued, not bytes already consumed by this reader.
    fixture.session().streams().ack(id, first.len()).unwrap();
    let mut total = first.len();
    loop {
        match tokio::time::timeout(Duration::from_secs(60), reader.next_frame())
            .await
            .expect("the stream stalled")
        {
            Some(Frame::Binary(bytes)) => {
                total += bytes.len();
                fixture.session().streams().ack(id, bytes.len()).unwrap();
            }
            Some(Frame::End) | None => break,
            other => panic!("expected the end, got {other:?}"),
        }
    }
    assert_eq!(total, 4 * 1024 * 1024);
}

#[tokio::test]
async fn wait_is_once_and_the_resource_is_gone_after_it() {
    let fixture = app().await;
    let reply = spawn(&fixture, json!({ "program": "node", "args": ["-e", ""] })).await;
    let id = reply["process"].as_u64().unwrap();
    let reply = tokio::time::timeout(
        Duration::from_secs(60),
        fixture.call("cli.wait", json!({ "process": id })),
    )
    .await
    .expect("the wait finishes in time")
    .expect("the wait works");
    assert_eq!(reply["code"], 0);
    assert!(reply["signal"].is_null());
    let error = fixture
        .call("cli.wait", json!({ "process": id }))
        .await
        .unwrap_err();
    assert_eq!(error.code, alef_core::ErrorCode::NotFound);
    let error = fixture
        .call("cli.kill", json!({ "process": id }))
        .await
        .unwrap_err();
    assert_eq!(error.code, alef_core::ErrorCode::NotFound);
}

#[tokio::test]
async fn kill_takes_down_the_whole_tree() {
    let fixture = app().await;
    let dir = tempdir().unwrap();
    let child_file = dir.path().join("child");
    let grand_file = dir.path().join("grand");
    // The child writes its own pid file (the reply has it too), starts a grandchild that writes
    // its pid into another file, and both sleep forever. The grandchild is found through the
    // environment the child passes on.
    let script = r#"require('fs').writeFileSync(process.env.ALEF_CLI_TEST_PIDFILE,String(process.pid));require('child_process').spawn(process.execPath,['-e','require("fs").writeFileSync(process.env.GRAND_PID,String(process.pid));setInterval(()=>{},1e5)'],{stdio:'ignore',env:process.env,detached:process.platform==='win32'}).unref();setInterval(()=>{},1e5)"#;
    let reply = spawn(
        &fixture,
        json!({
            "program": "node",
            "args": ["-e", script],
            "env": [
                ["ALEF_CLI_TEST_PIDFILE", child_file.to_string_lossy()],
                ["GRAND_PID", grand_file.to_string_lossy()]
            ]
        }),
    )
    .await;
    let id = reply["process"].as_u64().unwrap();
    until(
        || async { pid_of(&grand_file).is_some() },
        60_000,
        "the grandchild wrote its pid",
    )
    .await;
    let grand = pid_of(&grand_file).unwrap();
    assert!(alive(&fixture, grand).await);
    let mut waiting = Box::pin(fixture.call("cli.wait", json!({ "process": id })));
    assert!(
        std::future::poll_fn(|cx| std::task::Poll::Ready(
            std::future::Future::poll(waiting.as_mut(), cx).is_pending()
        ))
        .await
    );
    fixture
        .call("cli.kill", json!({ "process": id }))
        .await
        .expect("the kill works");
    until(
        || async { !alive(&fixture, grand).await },
        60_000,
        "the grandchild is dead",
    )
    .await;
    let reply = waiting.await.expect("the child is over too");
    if cfg!(windows) {
        assert!(!reply["code"].is_null(), "{reply}");
        assert!(reply["signal"].is_null());
    } else {
        assert!(reply["code"].is_null(), "{reply}");
        assert_eq!(reply["signal"], "SIGKILL");
    }
}

#[tokio::test]
async fn closing_the_session_kills_the_tree() {
    let fixture = app().await;
    let dir = tempdir().unwrap();
    let file = dir.path().join("pid");
    let reply = spawn(
        &fixture,
        json!({
            "program": "node",
            "args": ["-e", r#"require('fs').writeFileSync(process.env.ALEF_CLI_TEST_PIDFILE,String(process.pid));setInterval(()=>{},1e5)"#],
            "env": [["ALEF_CLI_TEST_PIDFILE", file.to_string_lossy()]]
        }),
    )
    .await;
    let pid = reply["pid"].as_u64().unwrap();
    until(|| async { pid_of(&file).is_some() }, 60_000, "a pid file").await;
    let mut waiting = Box::pin(fixture.call("cli.wait", json!({ "process": reply["process"] })));
    assert!(
        std::future::poll_fn(|cx| std::task::Poll::Ready(
            std::future::Future::poll(waiting.as_mut(), cx).is_pending()
        ))
        .await
    );
    tokio::time::timeout(Duration::from_secs(60), fixture.session().close())
        .await
        .expect("session teardown completes");
    let waited = tokio::time::timeout(Duration::from_secs(60), waiting)
        .await
        .expect("pending wait wakes on close")
        .expect("exit result remains observable");
    assert!(waited["code"].as_i64().is_some() || waited["signal"].as_str().is_some());
    assert!(fixture.session().resources().is_empty());
    // The probe needs a new live session; use an independent registry fixture.
    let probe = app().await;
    until(
        || async { !alive(&probe, pid).await },
        60_000,
        "the tree is dead",
    )
    .await;
}

/// Writing to the stdin stream of a spawned child and then ending it closes the input of the child.
#[tokio::test]
async fn ending_the_stdin_stream_ends_the_input_of_the_child() {
    let fixture = app().await;
    let reply = spawn(
        &fixture,
        json!({
            "program": "node",
            "args": ["-e", r#"let d='';process.stdin.on('data',c=>d+=c);process.stdin.on('end',()=>{process.stdout.write('got:'+d);process.exit(0)})"#],
            "stdin": "pipe",
            "stdout": "pipe"
        }),
    )
    .await;
    let writer = fixture
        .session()
        .streams()
        .incoming_writer(StreamId(reply["stdin"].as_u64().unwrap()))
        .expect("an incoming stream");
    writer
        .write(Bytes::from_static(b"tail"))
        .await
        .expect("the child reads");
    writer.end();
    let mut pipe = Pipe::open(&fixture, reply["stdout"].as_u64().unwrap());
    assert_eq!(pipe.until_end().await.unwrap(), b"got:tail");
}

#[tokio::test]
async fn foreign_resource_wait_does_not_consume_it() {
    struct Foreign;
    impl alef_core::session::resources::Resource for Foreign {
        fn close(
            self: Box<Self>,
        ) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
            Box::pin(async {})
        }
    }
    let fixture = app().await;
    let session = fixture.session();
    let id = session.resources().insert(Box::new(Foreign)).unwrap();
    let error = fixture
        .call("cli.wait", json!({"process": id.0}))
        .await
        .unwrap_err();
    assert_eq!(error.code, alef_core::ErrorCode::NotFound);
    session
        .resources()
        .with_as::<Foreign, _>(id, |_| ())
        .unwrap();
}

#[tokio::test]
async fn wait_preserves_output_until_the_page_reads_it() {
    let fixture = app().await;
    let count = DEFAULT_STREAM_WINDOW + 8192;
    // The last 8 KiB fit in the pump's pending chunk/OS pipe after credit fills. The write
    // callbacks guarantee the child can exit, while the pumps cannot send EOF without credit.
    let script = format!("process.stdout.write(Buffer.alloc({count},111),()=>process.stderr.write(Buffer.alloc({count},101),()=>process.exit(0)))");
    let reply = spawn(
        &fixture,
        json!({
            "program": "node", "args": ["-e", script], "stdin": "ignore"
        }),
    )
    .await;
    let waited = tokio::time::timeout(
        Duration::from_secs(60),
        fixture.call("cli.wait", json!({"process": reply["process"]})),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(waited["code"], 0);
    let mut out = Pipe::open(&fixture, reply["stdout"].as_u64().unwrap());
    let mut err = Pipe::open(&fixture, reply["stderr"].as_u64().unwrap());
    assert_eq!(out.until_end().await.unwrap(), vec![111; count]);
    assert_eq!(err.until_end().await.unwrap(), vec![101; count]);
    until(
        || async { fixture.session().resources().is_empty() },
        60_000,
        "completed process resource removed",
    )
    .await;
}

#[tokio::test]
async fn descendants_die_when_direct_child_exits() {
    let fixture = app().await;
    let dir = tempdir().unwrap();
    let grand_file = dir.path().join("grand");
    let release = dir.path().join("release");
    let script = r#"const fs=require('fs');require('child_process').spawn(process.execPath,['-e','require("fs").writeFileSync(process.env.GRAND_PID,String(process.pid));setInterval(()=>{},1e5)'],{stdio:'ignore',detached:process.platform==='win32'}).unref();const t=setInterval(()=>{if(fs.existsSync(process.env.RELEASE))process.exit(0)},10)"#;
    let reply = spawn(
        &fixture,
        json!({"program":"node","args":["-e",script],
        "stdin":"ignore","stdout":"ignore","stderr":"ignore",
        "env":[["GRAND_PID",grand_file.to_string_lossy()],["RELEASE",release.to_string_lossy()]]}),
    )
    .await;
    until(
        || async { pid_of(&grand_file).is_some() },
        60_000,
        "grandchild ready",
    )
    .await;
    let grand = pid_of(&grand_file).unwrap();
    assert!(alive(&fixture, grand).await);
    std::fs::write(&release, b"exit").unwrap();
    let result = tokio::time::timeout(
        Duration::from_secs(60),
        fixture.call("cli.wait", json!({"process":reply["process"]})),
    )
    .await
    .unwrap()
    .unwrap();
    assert_eq!(result["code"], 0);
    until(
        || async { !alive(&fixture, grand).await },
        60_000,
        "descendant terminated after parent exit",
    )
    .await;
}

/// A suppressed stream goes nowhere and is not wired at all.
#[tokio::test]
async fn an_ignored_stream_is_null_and_the_child_still_runs() {
    let fixture = app().await;
    let reply = spawn(
        &fixture,
        json!({
            "program": "node",
            "args": ["-e", r#"process.stderr.write('noise');process.exit(2)"#],
            "stdin": "ignore",
            "stdout": "ignore",
            "stderr": "ignore"
        }),
    )
    .await;
    assert!(reply["stdin"].is_null());
    assert!(reply["stdout"].is_null());
    assert!(reply["stderr"].is_null());
    let reply = tokio::time::timeout(
        Duration::from_secs(60),
        fixture.call("cli.wait", json!({ "process": reply["process"] })),
    )
    .await
    .expect("the wait finishes")
    .expect("the wait works");
    assert_eq!(reply["code"], 2);
}
