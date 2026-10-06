// SPDX-License-Identifier: MIT OR Apache-2.0
//! Document lifecycle: a reload or a closed window ends every resource, stream and token of the session.
use super::*;
use crate::{
    protocol::frame::{Frame, FrameDecoder},
    ErrorCode,
};

/// Opens a blocked outgoing stream (producer waiting for credit) and returns its live body.
async fn live_stream(fx: &Fixture, token: &str) -> FrameBody {
    let args = json!({"total": 8 * 1024 * 1024, "piece": 64 * 1024});
    let response = send(fx, call("test.flood", token, &args)).await;
    let id = body_json(&response)["stream"].as_u64().expect("id");
    let response = send(
        fx,
        request(Method::Get, &format!("stream/{id}")).token(token),
    )
    .await;
    match response.body {
        ResponseBody::Frames(body) => body,
        other => panic!("expected frames, got {other:?}"),
    }
}

async fn assert_session_is_gone(fx: &Fixture, old_token: &str, mut body: FrameBody) {
    assert!(
        fx.probe.resource_closed.load(Ordering::SeqCst),
        "resources closed"
    );
    let denied = send(fx, call("test.echo", old_token, &json!({}))).await;
    assert_eq!(denied.status, 403, "the old token is dead");
    let mut decoder = FrameDecoder::new();
    let mut last = None;
    while let Some(chunk) = bounded(body.next_chunk()).await {
        for frame in decoder.push(&chunk).expect("valid") {
            last = Some(frame);
        }
    }
    let last = last.expect("at least a terminal frame");
    assert!(
        matches!(&last, Frame::Error(e) if e.code == ErrorCode::Closed),
        "{last:?}"
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
        outcome.expect_err("producer must fail").code,
        ErrorCode::Closed
    );
}

#[tokio::test]
async fn a_reload_closes_resources_and_streams_and_kills_the_old_token() {
    let fx = fixture(config(Limits::default()));
    let old = open(&fx, WINDOW).await;
    let old_token = old.token().to_owned();
    assert_eq!(
        send(&fx, call("test.resource", &old_token, &json!({})))
            .await
            .status,
        200
    );
    assert!(
        !fx.probe.resource_closed.load(Ordering::SeqCst),
        "still open before the reload"
    );
    let body = live_stream(&fx, &old_token).await;
    let fresh = open(&fx, WINDOW).await;
    assert_session_is_gone(&fx, &old_token, body).await;
    let works = send(&fx, call("test.echo", fresh.token(), &json!({"new": 1}))).await;
    assert_eq!(works.status, 200, "the new document's token works");
}

#[tokio::test]
async fn closing_the_window_ends_the_session_and_hello_has_nothing_to_hand_out() {
    let fx = fixture(config(Limits::default()));
    let session = open(&fx, WINDOW).await;
    let token = session.token().to_owned();
    assert_eq!(
        send(&fx, call("test.resource", &token, &json!({})))
            .await
            .status,
        200
    );
    let body = live_stream(&fx, &token).await;
    bounded(fx.sessions.close_window(WINDOW)).await;
    assert_session_is_gone(&fx, &token, body).await;
    let hello = request(Method::Post, "call/runtime.hello").token(BOOTSTRAP);
    assert_eq!(send(&fx, hello).await.status, 404);
}

#[tokio::test]
async fn reloading_one_window_leaves_the_others_alone() {
    let fx = fixture(config(Limits::default()));
    let first = open(&fx, 1).await;
    let second = open(&fx, 2).await;
    let before = send(
        &fx,
        call("test.echo", second.token(), &json!({})).on_window(2),
    )
    .await;
    assert_eq!(before.status, 200);
    open(&fx, 1).await;
    assert!(!first.is_open());
    let after = send(
        &fx,
        call("test.echo", second.token(), &json!({})).on_window(2),
    )
    .await;
    assert_eq!(after.status, 200, "window 2 keeps its session");
}

fn token_of(response: &TransportResponse) -> String {
    assert_eq!(response.status, 200, "hello must succeed");
    body_json(response)["token"]
        .as_str()
        .expect("token")
        .to_owned()
}

#[tokio::test]
async fn hello_ties_the_session_to_the_calling_document() {
    let fx = fixture(config(Limits::default()));
    let hello = |document: u64| {
        request(Method::Post, "call/runtime.hello")
            .token(BOOTSTRAP)
            .in_document(document)
    };
    // The embedder never reported the first load: hello alone creates the session.
    let first = token_of(&send(&fx, hello(10)).await);
    let again = token_of(&send(&fx, hello(10)).await);
    assert_eq!(first, again, "same document, same token");
    let echo = |token: &str| call("test.echo", token, &json!({}));
    assert_eq!(send(&fx, echo(&first)).await.status, 200);

    // A newer document (reload, navigation) rotates the session; the old token dies at once.
    let second = token_of(&send(&fx, hello(11)).await);
    assert_ne!(first, second);
    assert_denied(
        &send(&fx, echo(&first)).await,
        "token of the replaced document",
    );
    assert_eq!(send(&fx, echo(&second)).await.status, 200);

    // A stale document cannot take the window back, and the live session is left alone.
    assert_denied(&send(&fx, hello(10)).await, "hello from a stale document");
    assert_eq!(send(&fx, echo(&second)).await.status, 200);
    assert_eq!(token_of(&send(&fx, hello(11)).await), second);

    // Window 0 means "no window": it never gets a session.
    assert_denied(
        &send(&fx, hello(12).on_window(0)).await,
        "hello without a window",
    );
    assert!(fx.sessions.current(0).is_none());

    // Windows are independent.
    let other = token_of(&send(&fx, hello(5).on_window(2)).await);
    assert_ne!(other, second);
    assert_eq!(send(&fx, echo(&second)).await.status, 200);
    assert_eq!(
        send(&fx, call("test.echo", &other, &json!({})).on_window(2))
            .await
            .status,
        200
    );
}
