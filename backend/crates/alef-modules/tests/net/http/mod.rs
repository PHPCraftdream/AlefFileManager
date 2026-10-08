// SPDX-License-Identifier: MIT OR Apache-2.0
//! `http` through the registry: a request and its answer, the body that goes up and the body that
//! comes down as streams, the redirects held against the scope again, the right the user substituted,
//! and `download` into a file.
use std::{path::Path, time::Duration};

use alef_core::{
    protocol::frame::Frame,
    registry::command::Reply,
    security::consent::{Consent, Decision, Right},
    AlefError, ErrorCode,
};
use bytes::Bytes;
use serde_json::{json, Value};

use crate::{
    common::Fixture,
    server::{big_byte, Server, BIG},
};

const MANIFEST: &str = include_str!("../../fixtures/app.ktav");

/// An application that may reach the given URL patterns and write below the given scopes.
fn manifest(http: &[String], write: &[String]) -> String {
    let list = |items: &[String]| format!("[ {} ]", items.join(", "));
    let text = MANIFEST
        .replace('\r', "")
        .replace("        http: []", &format!("        http: {}", list(http)))
        .replace(
            "        read: []\n        write: []",
            &format!("        read: []\n        write: {}", list(write)),
        );
    assert!(text.contains("http: [ ") || http.is_empty());
    text
}

async fn app(http: &[String]) -> Fixture {
    Fixture::new(Some(&manifest(http, &[])), &[]).await
}

/// The user chose a stand-in for what `scope` of `net.http` lets through.
fn substituting(scope: &str) -> Consent {
    let mut consent = Consent::undecided();
    consent.set(Right::scoped("net.http", scope), Decision::Substitute);
    consent
}

async fn ask(app: &Fixture, args: Value) -> Result<Value, AlefError> {
    app.call("http.request", args).await
}

async fn ask_with(app: &Fixture, args: Value, body: &[u8]) -> Result<Value, AlefError> {
    match app
        .call_reply("http.request", args, Some(Bytes::copy_from_slice(body)))
        .await?
    {
        Reply::Json(value) => Ok(value),
        other => panic!("expected JSON, got {other:?}"),
    }
}

async fn body_of(app: &Fixture, head: &Value) -> Vec<u8> {
    body_result(app, head).await.expect("the stream failed")
}

/// Everything the body of an answer carries, acknowledging as a page does, or the error it ended with.
async fn body_result(app: &Fixture, head: &Value) -> Result<Vec<u8>, AlefError> {
    let Some(id) = head["stream"].as_u64() else {
        return Ok(Vec::new());
    };
    let id = alef_core::ids::StreamId(id);
    let session = app.session();
    let mut reader = session.streams().reader(id).expect("an outgoing stream");
    let mut data = Vec::new();
    loop {
        match tokio::time::timeout(Duration::from_secs(30), reader.next_frame())
            .await
            .expect("the stream stalled")
        {
            Some(Frame::Binary(bytes)) => {
                session.streams().ack(id, bytes.len()).unwrap();
                data.extend_from_slice(&bytes);
            }
            Some(Frame::End) | None => return Ok(data),
            Some(Frame::Error(error)) => return Err(error),
            Some(Frame::Json(value)) => panic!("unexpected {value}"),
        }
    }
}

fn header<'a>(head: &'a Value, name: &str) -> Option<&'a str> {
    head["headers"]
        .as_array()?
        .iter()
        .find(|pair| pair[0] == name)
        .and_then(|pair| pair[1].as_str())
}

fn code(result: Result<Value, AlefError>) -> ErrorCode {
    result.expect_err("an error").code
}

/// What the server saw from the `from`-th request on, as `METHOD /path [body]`.
fn trail(server: &Server, from: usize) -> Vec<String> {
    server.requests()[from..]
        .iter()
        .map(|seen| {
            format!(
                "{} {} [{}]",
                seen.method,
                seen.path,
                String::from_utf8_lossy(&seen.body)
            )
        })
        .collect()
}

/// Waits (a few seconds at most) until a condition holds.
async fn until(mut condition: impl FnMut() -> bool) -> bool {
    for _ in 0..60 {
        if condition() {
            return true;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    condition()
}

mod download;
mod failures;
mod redirects;
mod serve;

#[tokio::test]
async fn an_answer_has_its_head_and_its_body_and_some_have_no_body() {
    let server = Server::start().await;
    let app = app(&[server.scope()]).await;
    let head = ask(&app, json!({ "url": server.url("/hello") }))
        .await
        .unwrap();
    assert_eq!(head["status"], 200);
    assert_eq!(head["statusText"], "OK");
    assert_eq!(head["redirected"], false);
    assert_eq!(head["url"], server.url("/hello"));
    assert_eq!(header(&head, "x-test"), Some("a"));
    assert_eq!(header(&head, "content-type"), Some("text/plain"));
    assert_eq!(body_of(&app, &head).await, b"hello");

    let posted = ask_with(
        &app,
        json!({ "url": server.url("/echo"), "method": "post" }),
        b"the body \xF0\x9F\x99\x82",
    )
    .await
    .unwrap();
    assert_eq!(
        header(&posted, "x-method"),
        Some("POST"),
        "the method is upper case"
    );
    assert_eq!(
        body_of(&app, &posted).await,
        "the body \u{1F642}".as_bytes()
    );

    let head_only = ask(
        &app,
        json!({ "url": server.url("/hello"), "method": "HEAD" }),
    )
    .await
    .unwrap();
    assert_eq!(head_only["stream"], Value::Null, "a HEAD has no body");
    let none = ask(&app, json!({ "url": server.url("/nobody") }))
        .await
        .unwrap();
    assert_eq!(
        (none["status"].as_u64(), none["stream"].clone()),
        (Some(204), Value::Null)
    );
    let missing = ask(&app, json!({ "url": server.url("/notfound") }))
        .await
        .unwrap();
    assert_eq!(
        missing["status"], 404,
        "a status of failure is an answer, not an error"
    );
    assert_eq!(body_of(&app, &missing).await, b"no such thing");
}

#[tokio::test]
async fn the_headers_the_page_sets_arrive_and_the_ones_that_are_the_clients_are_refused() {
    let server = Server::start().await;
    let app = app(&[server.scope()]).await;
    let head = ask(
        &app,
        json!({
            "url": server.url("/hello"),
            "headers": [["X-One", "1"], ["x-two", "2"], ["x-two", "3"], ["accept", "text/plain"]],
        }),
    )
    .await
    .unwrap();
    body_of(&app, &head).await;
    let seen = server.requests().pop().unwrap();
    assert_eq!(seen.headers["x-one"], "1");
    assert_eq!(
        seen.headers["x-two"], "2, 3",
        "both values of a header reach the server"
    );
    assert_eq!(seen.headers["accept"], "text/plain");
    assert!(
        seen.headers["user-agent"].starts_with("Alef/"),
        "{:?}",
        seen.headers
    );
    let own = ask(
        &app,
        json!({ "url": server.url("/hello"), "headers": [["user-agent", "mine"]] }),
    )
    .await
    .unwrap();
    body_of(&app, &own).await;
    assert_eq!(
        server.requests().pop().unwrap().headers["user-agent"],
        "mine"
    );

    let before = server.requests().len();
    for name in [
        "Host",
        "content-length",
        "Transfer-Encoding",
        "connection",
        "upgrade",
        "te",
        "proxy-authorization",
    ] {
        let refused = ask(
            &app,
            json!({ "url": server.url("/hello"), "headers": [[name, "x"]] }),
        )
        .await;
        assert_eq!(code(refused), ErrorCode::InvalidArgument, "{name}");
    }
    for bad in [
        json!({ "url": server.url("/hello"), "headers": [["bad name", "x"]] }),
        json!({ "url": server.url("/hello"), "headers": [["x-ok", "line\nbreak"]] }),
        json!({ "url": server.url("/hello"), "method": "CONNECT" }),
        json!({ "url": server.url("/hello"), "method": "no way" }),
        json!({ "url": "ftp://127.0.0.1/hello" }),
        json!({ "url": "not a url" }),
        json!({ "url": format!("http://user:secret@127.0.0.1:{}/hello", server.address.port()) }),
        json!({ "url": server.url("/hello"), "unknown": true }),
    ] {
        assert!(ask(&app, bad.clone()).await.is_err(), "{bad}");
    }
    assert_eq!(server.requests().len(), before, "nothing refused was sent");
}

#[tokio::test]
async fn an_address_outside_the_scope_is_denied_before_anything_is_sent() {
    let server = Server::start().await;
    let other = Server::start().await;
    let app = app(&[server.scope()]).await;
    for url in [
        other.url("/hello"),
        server.url("/hello").replace("127.0.0.1", "localhost"),
        "https://example.com/".to_owned(),
    ] {
        let error = ask(&app, json!({ "url": url })).await.unwrap_err();
        assert_eq!(error.code, ErrorCode::PermissionDenied, "{url}");
        assert_eq!(error.details, Some(json!({ "permission": "net.http" })));
    }
    assert!(server.requests().is_empty() && other.requests().is_empty());
    let none = Fixture::new(None, &[]).await;
    assert_eq!(
        code(ask(&none, json!({ "url": server.url("/hello") })).await),
        ErrorCode::PermissionDenied
    );
}

#[tokio::test]
async fn a_big_body_goes_up_and_comes_down_as_a_stream_and_arrives_whole() {
    let server = Server::start().await;
    let app = app(&[server.scope()]).await;
    let started = app
        .call(
            "http.start",
            json!({ "url": server.url("/sum"), "method": "POST" }),
        )
        .await
        .unwrap();
    let pattern: Vec<u8> = (0..BIG).map(big_byte).collect();
    let writer = app
        .session()
        .streams()
        .incoming_writer(alef_core::ids::StreamId(
            started["upload"].as_u64().unwrap(),
        ))
        .expect("an incoming stream");
    for piece in pattern.chunks(256 * 1024) {
        writer.write(Bytes::copy_from_slice(piece)).await.unwrap();
    }
    writer.end();
    let head = app
        .call("http.response", json!({ "request": started["request"] }))
        .await
        .unwrap();
    assert_eq!(head["status"], 200);
    assert_eq!(
        header(&head, "x-length"),
        Some(BIG.to_string().as_str()),
        "all of it arrived"
    );
    let expected = pattern.iter().enumerate().fold(0_u64, |sum, (at, byte)| {
        sum.wrapping_add((at as u64 + 1) * u64::from(*byte))
    });
    assert_eq!(
        body_of(&app, &head).await,
        expected.to_string().as_bytes(),
        "and in order"
    );
    let again = app
        .call("http.response", json!({ "request": started["request"] }))
        .await;
    assert_eq!(
        code(again),
        ErrorCode::NotFound,
        "an answer is asked for once"
    );

    let down = ask(&app, json!({ "url": server.url("/big") }))
        .await
        .unwrap();
    assert_eq!(
        header(&down, "content-length"),
        Some(BIG.to_string().as_str())
    );
    let received = body_of(&app, &down).await;
    assert_eq!(received.len(), BIG);
    assert!(
        received
            .iter()
            .enumerate()
            .all(|(at, byte)| *byte == big_byte(at)),
        "the pattern came whole"
    );
}

#[tokio::test]
async fn a_slow_server_times_out_and_a_server_that_is_not_there_is_the_network() {
    let server = Server::start().await;
    let app = app(&[server.scope(), "http://127.0.0.1:9/*".to_owned()]).await;
    let started = std::time::Instant::now();
    let slow = ask(
        &app,
        json!({ "url": server.url("/slow"), "timeoutMs": 500 }),
    )
    .await;
    assert_eq!(code(slow), ErrorCode::Timeout);
    assert!(
        until(|| server.cut().contains(&"/slow".to_owned())).await,
        "the connection of a request that timed out was left open"
    );
    assert!(
        started.elapsed() < Duration::from_secs(2),
        "{:?}",
        started.elapsed()
    );
    let gone = ask(
        &app,
        json!({ "url": "http://127.0.0.1:9/nothing", "timeoutMs": 5000 }),
    )
    .await;
    assert_eq!(code(gone), ErrorCode::Network);
}

#[tokio::test]
async fn what_the_user_substituted_is_a_dead_network() {
    let server = Server::start().await;
    let app = Fixture::new(Some(&manifest(&[server.scope()], &[])), &[])
        .await
        .with_consent(substituting(&server.scope()));
    let started = std::time::Instant::now();
    let dead = ask(
        &app,
        json!({ "url": server.url("/hello"), "timeoutMs": 300 }),
    )
    .await;
    assert_eq!(code(dead), ErrorCode::Timeout);
    assert!(
        started.elapsed() >= Duration::from_millis(250),
        "it hung: {:?}",
        started.elapsed()
    );
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "it hung for the time the page asked, not for the default: {:?}",
        started.elapsed()
    );
    let begun = app
        .call(
            "http.start",
            json!({ "url": server.url("/echo"), "method": "POST", "timeoutMs": 300 }),
        )
        .await
        .unwrap();
    let writer = app
        .session()
        .streams()
        .incoming_writer(alef_core::ids::StreamId(begun["upload"].as_u64().unwrap()))
        .expect("an incoming stream");
    writer
        .write(Bytes::from_static(b"into the void"))
        .await
        .unwrap();
    writer.end();
    let answer = app
        .call("http.response", json!({ "request": begun["request"] }))
        .await;
    assert_eq!(code(answer), ErrorCode::Timeout);
    assert!(
        server.requests().is_empty(),
        "nothing reached the server: {:?}",
        server.requests()
    );
}
