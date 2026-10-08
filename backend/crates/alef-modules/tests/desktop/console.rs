// SPDX-License-Identifier: MIT OR Apache-2.0
//! The standard streams of a console utility through the registry: stdin as a stream that ends with it, stdout
//! and stderr that carry what the page writes whole and in order, the wait at the end of the process, and the
//! application that is no console utility.
use std::{sync::Arc, time::Duration};

use alef_core::{ids::StreamId, protocol::frame::Frame, session::Session, ErrorCode};
use alef_modules::Console;
use bytes::Bytes;
use serde_json::Value;
use tokio::io::{duplex, AsyncReadExt, AsyncWriteExt, DuplexStream};

use crate::common::Fixture;

/// The far ends of a console: what the process would be given and what it would give.
struct Ends {
    stdin: DuplexStream,
    stdout: DuplexStream,
    stderr: DuplexStream,
}

fn console(capacity: usize) -> (Console, Ends) {
    let (stdin_near, stdin) = duplex(capacity);
    let (stdout_near, stdout) = duplex(capacity);
    let (stderr_near, stderr) = duplex(capacity);
    (
        Console::with(stdin_near, stdout_near, stderr_near),
        Ends {
            stdin,
            stdout,
            stderr,
        },
    )
}

fn pattern(count: usize) -> Vec<u8> {
    (0..count).map(|at| (at * 31 + (at >> 8)) as u8).collect()
}

async fn open(fixture: &Fixture, command: &str) -> StreamId {
    let reply = fixture.call(command, Value::Null).await.expect(command);
    StreamId(reply["stream"].as_u64().unwrap())
}

/// Everything a stream of the runtime carries until it ends, acknowledged as a page does.
async fn read_all(session: &Arc<Session>, id: StreamId) -> Vec<u8> {
    let mut reader = session.streams().reader(id).expect("an outgoing stream");
    let mut bytes = Vec::new();
    loop {
        match tokio::time::timeout(Duration::from_secs(20), reader.next_frame())
            .await
            .expect("the stream stalled")
        {
            Some(Frame::Binary(piece)) => {
                session.streams().ack(id, piece.len()).unwrap();
                bytes.extend_from_slice(&piece);
            }
            Some(Frame::End) | None => return bytes,
            other => panic!("unexpected {other:?}"),
        }
    }
}

/// Writes pieces to a stream of the page, then ends it.
async fn write_all(session: &Arc<Session>, id: StreamId, data: &[u8], piece: usize) {
    let writer = session
        .streams()
        .incoming_writer(id)
        .expect("an incoming stream");
    for part in data.chunks(piece) {
        writer.write(Bytes::copy_from_slice(part)).await.unwrap();
    }
    writer.end();
}

#[tokio::test]
async fn an_application_that_is_no_console_utility_has_no_standard_streams() {
    let fixture = Fixture::new(None, &[]).await;
    for command in ["app.stdin", "app.stdout", "app.stderr"] {
        let error = fixture.call(command, Value::Null).await.expect_err(command);
        assert_eq!(error.code, ErrorCode::NotAvailable, "{command}");
    }
}

#[tokio::test]
async fn stdin_comes_to_the_page_as_a_stream_and_ends_with_it() {
    let (console, mut ends) = console(64 * 1024);
    let fixture = Fixture::new_console(console, None).await;
    let session = fixture.session();
    let id = open(&fixture, "app.stdin").await;
    let sent = pattern(700 * 1024);
    let feeding = {
        let sent = sent.clone();
        tokio::spawn(async move {
            ends.stdin.write_all(&sent).await.unwrap();
            // The end of stdin is the end of the far side.
        })
    };
    let got = read_all(&session, id).await;
    feeding.await.unwrap();
    assert!(got == sent, "stdin came whole and in order");
    let again = fixture.call("app.stdin", Value::Null).await.unwrap_err();
    assert_eq!(again.code, ErrorCode::Busy, "stdin is taken once");
}

#[tokio::test]
async fn stdout_and_stderr_carry_what_the_page_writes_whole_and_in_order_each_on_its_own() {
    let (console, mut ends) = console(64 * 1024);
    let fixture = Fixture::new_console(console, None).await;
    let session = fixture.session();
    let (out_id, err_id) = (
        open(&fixture, "app.stdout").await,
        open(&fixture, "app.stderr").await,
    );
    let (out, err) = (
        pattern(3 * 1024 * 1024),
        b"a few words of an error".to_vec(),
    );
    let collecting = tokio::spawn(async move {
        let (mut out_bytes, mut err_bytes) = (Vec::new(), Vec::new());
        tokio::join!(
            async { ends.stdout.read_to_end(&mut out_bytes).await.unwrap() },
            async { ends.stderr.read_to_end(&mut err_bytes).await.unwrap() },
        );
        (out_bytes, err_bytes)
    });
    write_all(&session, out_id, &out, 256 * 1024).await;
    write_all(&session, err_id, &err, 7).await;
    fixture.context.console.as_ref().unwrap().drain().await;
    drop(fixture);
    let (got_out, got_err) = tokio::time::timeout(Duration::from_secs(20), collecting)
        .await
        .expect("the far ends stay open")
        .unwrap();
    assert!(got_out == out, "stdout came whole and in order");
    assert_eq!(got_err, err, "and stderr came on its own");
}

#[tokio::test]
async fn the_end_of_the_process_waits_for_what_the_page_wrote_to_leave() {
    let (console, mut ends) = console(16);
    let fixture = Fixture::new_console(console.clone(), None).await;
    let session = fixture.session();
    let id = open(&fixture, "app.stdout").await;
    let data = pattern(100 * 1024);
    // The page writes on its own: with nothing read from the far end its writes wait for credit.
    let writing = {
        let (session, data) = (session.clone(), data.clone());
        tokio::spawn(async move { write_all(&session, id, &data, 4096).await })
    };
    let draining = tokio::spawn(async move { console.drain().await });
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        !draining.is_finished(),
        "nothing has been read from the far end, so the bytes have not left"
    );
    let mut got = vec![0_u8; data.len()];
    let half = data.len() / 2;
    tokio::time::timeout(
        Duration::from_secs(20),
        ends.stdout.read_exact(&mut got[..half]),
    )
    .await
    .expect("the first bytes arrive on the far end")
    .unwrap();
    tokio::time::sleep(Duration::from_millis(300)).await;
    assert!(
        !draining.is_finished(),
        "half of the bytes has not left yet"
    );
    tokio::time::timeout(
        Duration::from_secs(20),
        ends.stdout.read_exact(&mut got[half..]),
    )
    .await
    .expect("the rest arrives on the far end")
    .unwrap();
    tokio::time::timeout(Duration::from_secs(10), draining)
        .await
        .expect("it waits no longer than the bytes need")
        .unwrap();
    writing.await.unwrap();
    assert!(got == data);
}

#[tokio::test]
async fn an_output_that_is_gone_closes_the_stream_of_the_page() {
    let (console, ends) = console(1024);
    let fixture = Fixture::new_console(console, None).await;
    let session = fixture.session();
    let id = open(&fixture, "app.stdout").await;
    drop(ends.stdout);
    let writer = session.streams().incoming_writer(id).unwrap();
    let mut failed = false;
    for _ in 0..200 {
        if writer
            .write(Bytes::from_static(b"nobody reads"))
            .await
            .is_err()
        {
            failed = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    assert!(failed, "the page kept writing to a pipe nobody reads");
}

/// Whether the pipe of stdin gets closed on the far side soon: a write to a pipe nobody reads fails.
async fn stdin_gets_closed(stdin: &mut DuplexStream) -> bool {
    for _ in 0..100 {
        match tokio::time::timeout(Duration::from_millis(100), stdin.write_all(b"x")).await {
            Ok(Err(_)) => return true,
            _ => tokio::time::sleep(Duration::from_millis(20)).await,
        }
    }
    false
}

#[tokio::test]
async fn the_end_of_the_process_stops_reading_stdin() {
    let (console, mut ends) = console(1024);
    let fixture = Fixture::new_console(console.clone(), None).await;
    open(&fixture, "app.stdin").await;
    console.drain().await;
    assert!(
        stdin_gets_closed(&mut ends.stdin).await,
        "stdin was still being read after the end"
    );
}

#[tokio::test]
async fn a_stdin_stream_the_page_closed_is_no_longer_read() {
    let (console, mut ends) = console(1024);
    let fixture = Fixture::new_console(console, None).await;
    let session = fixture.session();
    let id = open(&fixture, "app.stdin").await;
    ends.stdin.write_all(b"first").await.unwrap();
    let mut reader = session.streams().reader(id).expect("an outgoing stream");
    let frame = tokio::time::timeout(Duration::from_secs(20), reader.next_frame())
        .await
        .expect("the first piece arrives");
    assert!(matches!(frame, Some(Frame::Binary(_))));
    session.streams().close(id).unwrap();
    ends.stdin.write_all(b"second").await.unwrap();
    assert!(
        stdin_gets_closed(&mut ends.stdin).await,
        "stdin was still being read for a page that closed its stream"
    );
}
