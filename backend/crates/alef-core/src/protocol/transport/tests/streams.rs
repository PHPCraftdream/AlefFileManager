// SPDX-License-Identifier: MIT OR Apache-2.0
//! Stream routes: frame delivery, credit flow control, cancellation and page-to-runtime uploads.
use super::*;
use crate::{
    ids::StreamId,
    protocol::{
        call::Limits,
        frame::{Frame, FrameDecoder},
    },
};

const WINDOW_BYTES: usize = 1024 * 1024;

async fn start(fx: &Fixture, token: &str, command: &str, args: &Value) -> u64 {
    let response = send(fx, call(command, token, args)).await;
    assert_eq!(response.status, 200, "{command}");
    body_json(&response)["stream"].as_u64().expect("stream id")
}

fn stream_get(token: &str, id: u64) -> TransportRequest {
    request(Method::Get, &format!("stream/{id}")).token(token)
}

async fn frames_of(fx: &Fixture, token: &str, id: u64) -> FrameBody {
    let response = send(fx, stream_get(token, id)).await;
    assert_eq!(response.status, 200);
    assert_eq!(
        response.header("content-type"),
        Some("application/vnd.alef.frames")
    );
    match response.body {
        ResponseBody::Frames(body) => body,
        other => panic!("expected frames, got {other:?}"),
    }
}

async fn drain(body: &mut FrameBody) -> Vec<Frame> {
    let mut decoder = FrameDecoder::new();
    let mut frames = Vec::new();
    while let Some(chunk) = bounded(body.next_chunk()).await {
        frames.extend(decoder.push(&chunk).expect("valid frames"));
    }
    frames
}

fn ctl(token: &str, command: &str, args: &Value) -> TransportRequest {
    call(&format!("runtime.stream.{command}"), token, args)
}

#[tokio::test]
async fn an_outgoing_stream_delivers_its_frames_in_order_exactly_once() {
    let fx = fixture(config(Limits::default()));
    let session = open(&fx, WINDOW).await;
    let token = session.token();
    let id = start(&fx, token, "test.out", &json!({})).await;
    let mut body = frames_of(&fx, token, id).await;
    let frames = drain(&mut body).await;
    assert_eq!(
        frames,
        vec![
            Frame::Json(json!({"hello": 1})),
            Frame::Binary(Bytes::from_static(b"abc")),
            Frame::End
        ]
    );
    assert!(
        body.next_chunk().await.is_none(),
        "nothing after the terminal frame"
    );
    assert_eq!(
        send(&fx, stream_get(token, id)).await.status,
        404,
        "reader is once-only"
    );
    let other = open(&fx, 2).await;
    let foreign = request(Method::Get, &format!("stream/{id}")).token(other.token());
    assert_eq!(
        send(&fx, foreign).await.status,
        403,
        "another window's token"
    );
    let anonymous = request(Method::Get, &format!("stream/{id}"));
    assert_eq!(send(&fx, anonymous).await.status, 403);
    for bad in ["9999", "abc", "+1", "-1", "1 ", ""] {
        let response = send(
            &fx,
            request(Method::Get, &format!("stream/{bad}")).token(token),
        )
        .await;
        assert_eq!(response.status, 404, "{bad:?}");
    }
    let producer = fx
        .probe
        .producers
        .lock()
        .expect("producers")
        .pop()
        .expect("producer");
    bounded(producer)
        .await
        .expect("join")
        .expect("producer finished cleanly");
}

/// Pulls frames until `target` payload bytes arrived, then proves the producer is stuck.
/// Returns the highest outstanding credit seen while reading. The last window of a finite stream is
/// followed by `End` (no credit needed), so `blocked_after` is false there.
async fn fill_window(
    session: &Session,
    id: u64,
    body: &mut FrameBody,
    decoder: &mut FrameDecoder,
    got: &mut usize,
    target: usize,
    blocked_after: bool,
) -> usize {
    let mut peak = 0;
    while *got < target {
        let chunk = bounded(body.next_chunk()).await.expect("frames");
        for frame in decoder.push(&chunk).expect("valid") {
            match frame {
                Frame::Binary(bytes) => *got += bytes.len(),
                other => panic!("unexpected {other:?}"),
            }
        }
        peak = peak.max(session.streams().outstanding(StreamId(id)).expect("stream"));
    }
    if blocked_after {
        let silent = tokio::time::timeout(Duration::from_millis(100), body.next_chunk()).await;
        assert!(
            silent.is_err(),
            "the producer must be blocked once the window is used up"
        );
    }
    peak
}

#[tokio::test]
async fn the_credit_window_is_never_exceeded_and_acks_resume_the_flow() {
    let fx = fixture(config(Limits::default()));
    let session = open(&fx, WINDOW).await;
    let token = session.token();
    let total = 8 * WINDOW_BYTES;
    let id = start(
        &fx,
        token,
        "test.flood",
        &json!({"total": total, "piece": 64 * 1024}),
    )
    .await;
    let mut body = frames_of(&fx, token, id).await;
    let mut decoder = FrameDecoder::new();
    let (mut got, mut acked, mut peak, mut stalls) = (0usize, 0usize, 0usize, 0usize);
    while got < total {
        let target = acked + WINDOW_BYTES;
        let blocked_after = target < total;
        let seen = fill_window(
            &session,
            id,
            &mut body,
            &mut decoder,
            &mut got,
            target,
            blocked_after,
        );
        peak = peak.max(seen.await);
        assert_eq!(
            session.streams().outstanding(StreamId(id)),
            Some(WINDOW_BYTES)
        );
        stalls += 1;
        let ack = ctl(token, "ack", &json!({"id": id, "bytes": got - acked}));
        assert_eq!(send(&fx, ack).await.status, 200);
        acked = got;
    }
    assert_eq!(got, total);
    assert!(
        peak <= WINDOW_BYTES,
        "outstanding {peak} exceeded the window"
    );
    assert_eq!(stalls, 8, "one stall per window of 1 MiB");
    let tail = drain(&mut body).await;
    assert_eq!(tail, vec![Frame::End]);
}

#[tokio::test]
async fn close_releases_a_blocked_producer_and_ends_the_stream_with_closed() {
    let fx = fixture(config(Limits::default()));
    let session = open(&fx, WINDOW).await;
    let token = session.token();
    let id = start(
        &fx,
        token,
        "test.flood",
        &json!({"total": 8 * WINDOW_BYTES, "piece": 64 * 1024}),
    )
    .await;
    let mut body = frames_of(&fx, token, id).await;
    let (mut decoder, mut got) = (FrameDecoder::new(), 0usize);
    fill_window(
        &session,
        id,
        &mut body,
        &mut decoder,
        &mut got,
        WINDOW_BYTES,
        true,
    )
    .await;
    let began = std::time::Instant::now();
    assert_eq!(
        send(&fx, ctl(token, "close", &json!({"id": id})))
            .await
            .status,
        200
    );
    let producer = fx
        .probe
        .producers
        .lock()
        .expect("producers")
        .pop()
        .expect("producer");
    let outcome = bounded(producer).await.expect("join");
    assert_eq!(
        outcome.expect_err("send must fail").code,
        crate::ErrorCode::Closed
    );
    let rest = drain(&mut body).await;
    let last = rest.last().expect("a terminal frame");
    assert!(
        matches!(last, Frame::Error(e) if e.code == crate::ErrorCode::Closed),
        "{last:?}"
    );
    assert!(
        began.elapsed() < Duration::from_secs(1),
        "abort must not wait for acks"
    );
}

#[tokio::test]
async fn uploads_apply_backpressure_end_cleanly_and_close_with_an_error() {
    let limits = Limits {
        stream_window: 512,
        chunk_size: 256,
        ..Limits::default()
    };
    let fx = fixture(config(limits));
    let session = open(&fx, WINDOW).await;
    let token = session.token().to_owned();
    let id = start(&fx, &token, "test.up", &json!({})).await;
    let mut reader = fx
        .probe
        .readers
        .lock()
        .expect("readers")
        .pop()
        .expect("reader");
    let write = |id: u64, fill: u8, len: usize| {
        request(Method::Post, "call/runtime.stream.write")
            .token(&token)
            .octets(Bytes::from(vec![fill; len]), &json!({"id": id}))
    };
    for fill in [1u8, 2] {
        assert_eq!(send(&fx, write(id, fill, 256)).await.status, 200);
    }
    let third = {
        let transport = fx.transport.clone();
        let request = write(id, 3, 256);
        tokio::spawn(async move { transport.handle(request).await })
    };
    let mut third = third;
    assert!(
        tokio::time::timeout(Duration::from_millis(100), &mut third)
            .await
            .is_err(),
        "the third chunk must wait for the reader"
    );
    let first = bounded(reader.recv()).await.expect("chunk").expect("ok");
    assert_eq!(first[0], 1);
    assert_eq!(bounded(third).await.expect("join").status, 200);
    assert_eq!(
        send(&fx, ctl(&token, "end", &json!({"id": id})))
            .await
            .status,
        200
    );
    let mut rest = Vec::new();
    while let Some(item) = bounded(reader.recv()).await {
        rest.push(item.expect("clean end")[0]);
    }
    assert_eq!(rest, vec![2, 3]);
    let late = send(&fx, write(id, 9, 8)).await;
    assert_eq!(late.status, 410, "writes after the end fail with CLOSED");
    let aborted = start(&fx, &token, "test.up", &json!({})).await;
    let mut reader = fx
        .probe
        .readers
        .lock()
        .expect("readers")
        .pop()
        .expect("reader");
    assert_eq!(
        send(&fx, ctl(&token, "close", &json!({"id": aborted})))
            .await
            .status,
        200
    );
    let item = bounded(reader.recv()).await.expect("terminal item");
    assert_eq!(
        item.expect_err("close is an error").code,
        crate::ErrorCode::Closed
    );
    assert!(bounded(reader.recv()).await.is_none());
}

#[tokio::test]
async fn stream_control_commands_validate_their_input() {
    let limits = Limits {
        stream_window: 512,
        chunk_size: 256,
        ..Limits::default()
    };
    let fx = fixture(config(limits));
    let session = open(&fx, WINDOW).await;
    let token = session.token();
    let id = start(&fx, token, "test.up", &json!({})).await;
    let oversize = request(Method::Post, "call/runtime.stream.write")
        .token(token)
        .octets(Bytes::from(vec![0; 257]), &json!({"id": id}));
    assert_eq!(
        send(&fx, oversize).await.status,
        400,
        "chunk above chunkSize"
    );
    let no_body = request(Method::Post, "call/runtime.stream.write")
        .token(token)
        .json(&json!({"id": id}));
    assert_eq!(
        send(&fx, no_body).await.status,
        400,
        "a chunk body is required"
    );
    let unknown = request(Method::Post, "call/runtime.stream.write")
        .token(token)
        .octets(Bytes::from_static(b"x"), &json!({"id": 424242}));
    assert_eq!(send(&fx, unknown).await.status, 404);
    assert_eq!(
        send(&fx, ctl(token, "ack", &json!({"id": 424242, "bytes": 1})))
            .await
            .status,
        404
    );
    assert_eq!(
        send(&fx, ctl(token, "ack", &json!({"id": id})))
            .await
            .status,
        400,
        "missing field"
    );
    let extra = ctl(token, "close", &json!({"id": id, "x": 1}));
    assert_eq!(send(&fx, extra).await.status, 400, "unknown field");
    let anonymous = request(Method::Post, "call/runtime.stream.close").json(&json!({"id": id}));
    assert_eq!(send(&fx, anonymous).await.status, 403);
}
