// SPDX-License-Identifier: MIT OR Apache-2.0
//! `http.serve` through the registry, with a client of HTTP on the other side: what a request is for the
//! page, how it answers, what the server lets in (`held`), the folder it serves by itself (`files`), TLS, the
//! rights.
use std::{
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use alef_core::{
    ids::{ResourceId, StreamId},
    registry::command::Reply as Answer,
    security::consent::{Consent, Decision, Right},
    AlefError, ErrorCode,
};
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::{HeaderMap, Request, StatusCode};
use hyper_util::{
    client::legacy::Client,
    rt::{TokioExecutor, TokioIo},
};
use rustls::pki_types::{pem::PemObject, CertificateDer};
use serde_json::{json, Value};
use tokio::net::{TcpListener, TcpStream};

use crate::{
    common::Fixture,
    server::{big_byte, BIG},
    shared::{manifest::manifest, pipe::Pipe, tls},
};

type Reply = (StatusCode, HeaderMap, Vec<u8>);

const LISTEN: &str = "listen:127.0.0.1:*";

/// The manifest of an application that may take ports on the loopback and read the given scopes.
fn manifest_reading(readable: &[String]) -> String {
    let text = manifest(&[], &[LISTEN]);
    if readable.is_empty() {
        return text;
    }
    let reading = text.replace(
        "        read: []\n        write: []",
        &format!(
            "        read: [ {} ]\n        write: []",
            readable.join(", ")
        ),
    );
    assert_ne!(reading, text, "the manifest has a place for the scopes");
    reading
}

async fn app_for(readable: &[String]) -> Fixture {
    Fixture::new(Some(&manifest_reading(readable)), &[]).await
}

/// A server of the page, as the page has it.
struct Serving {
    id: u64,
    requests: Pipe,
    port: u16,
    reply: Value,
}

impl Serving {
    fn url(&self, path: &str) -> String {
        format!("http://127.0.0.1:{}{path}", self.port)
    }
}

async fn serve(app: &Fixture, extra: Value) -> Result<Serving, AlefError> {
    let mut args = json!({ "port": 0 });
    for (name, value) in extra.as_object().into_iter().flatten() {
        args[name] = value.clone();
    }
    let reply = app.call("http.serve", args).await?;
    Ok(Serving {
        id: reply["server"].as_u64().unwrap(),
        requests: Pipe::open(app, reply["requests"].as_u64().unwrap()),
        port: reply["address"]["port"].as_u64().unwrap() as u16,
        reply,
    })
}

/// A request as the page is told of it.
#[derive(Debug)]
struct Seen {
    id: u64,
    method: String,
    url: String,
    headers: Vec<(String, String)>,
    body: Option<u64>,
}

impl Seen {
    /// Every value of the header, in the order they came.
    fn all(&self, name: &str) -> Vec<&str> {
        self.headers
            .iter()
            .filter(|(header, _)| header == name)
            .map(|(_, value)| value.as_str())
            .collect()
    }

    fn one(&self, name: &str) -> &str {
        let values = self.all(name);
        assert_eq!(values.len(), 1, "{name} came {} times", values.len());
        values[0]
    }
}

async fn next_request(requests: &mut Pipe) -> Option<Seen> {
    let frame = requests.json().await?;
    Some(Seen {
        id: frame["id"].as_u64().unwrap(),
        method: frame["method"].as_str().unwrap().to_owned(),
        url: frame["url"].as_str().unwrap().to_owned(),
        headers: frame["headers"]
            .as_array()
            .unwrap()
            .iter()
            .map(|pair| {
                (
                    pair[0].as_str().unwrap().to_owned(),
                    pair[1].as_str().unwrap().to_owned(),
                )
            })
            .collect(),
        body: frame["body"].as_u64(),
    })
}

async fn respond(
    app: &Fixture,
    id: u64,
    status: u16,
    headers: &[(&str, &str)],
    body: &[u8],
) -> Result<Value, AlefError> {
    let pairs: Vec<[&str; 2]> = headers
        .iter()
        .map(|(name, value)| [*name, *value])
        .collect();
    match app
        .call_reply(
            "http.respond",
            json!({ "request": id, "status": status, "headers": pairs }),
            (!body.is_empty()).then(|| Bytes::copy_from_slice(body)),
        )
        .await?
    {
        Answer::Json(value) => Ok(value),
        other => panic!("expected JSON, got {other:?}"),
    }
}

/// A request of a client of HTTP, which any `Host` and `Origin` can be given.
async fn fetch(
    method: &'static str,
    url: String,
    headers: Vec<(&'static str, String)>,
    body: Vec<u8>,
) -> Result<Reply, String> {
    let client = Client::builder(TokioExecutor::new()).build_http::<Full<Bytes>>();
    let mut builder = Request::builder().method(method).uri(url);
    for (name, value) in headers {
        builder = builder.header(name, value);
    }
    let request = builder
        .body(Full::new(Bytes::from(body)))
        .map_err(|e| e.to_string())?;
    let response = client.request(request).await.map_err(|e| e.to_string())?;
    let (parts, body) = response.into_parts();
    let bytes = body.collect().await.map_err(|e| e.to_string())?.to_bytes();
    Ok((parts.status, parts.headers, bytes.to_vec()))
}

fn plain_get(url: String) -> tokio::task::JoinHandle<Result<Reply, String>> {
    tokio::spawn(fetch("GET", url, Vec::new(), Vec::new()))
}

/// No frame comes for the page in a moment.
async fn nothing_comes(requests: &mut Pipe) -> bool {
    tokio::time::timeout(Duration::from_millis(250), requests.frame())
        .await
        .is_err()
}

#[tokio::test]
async fn a_request_goes_to_the_page_as_a_frame_and_the_answer_goes_back_to_the_client() {
    let app = app_for(&[]).await;
    let mut served = serve(&app, json!({})).await.unwrap();
    assert_eq!(served.reply["address"]["host"], "127.0.0.1");
    assert_eq!(served.reply["secure"], false);
    let call = tokio::spawn(fetch(
        "GET",
        served.url("/hello?x=1"),
        vec![
            ("x-one", "1".to_owned()),
            ("accept", "text/plain".to_owned()),
            ("x-dup", "1".to_owned()),
            ("x-dup", "2".to_owned()),
        ],
        Vec::new(),
    ));
    let seen = next_request(&mut served.requests).await.expect("a request");
    assert_eq!(
        (seen.method.as_str(), seen.url.as_str()),
        ("GET", "/hello?x=1")
    );
    assert_eq!(seen.one("x-one"), "1");
    assert_eq!(seen.one("accept"), "text/plain");
    assert_eq!(seen.one("host"), format!("127.0.0.1:{}", served.port));
    assert_eq!(
        seen.all("x-dup"),
        ["1", "2"],
        "a header that repeats comes as it was"
    );
    assert_eq!(seen.body, None, "a request without a body has no stream");
    respond(
        &app,
        seen.id,
        201,
        &[("x-a", "b"), ("set-cookie", "a=1"), ("set-cookie", "b=2")],
        b"hi",
    )
    .await
    .unwrap();
    let (status, headers, body) = call.await.unwrap().unwrap();
    assert_eq!(status, 201);
    assert_eq!(headers["x-a"], "b");
    assert_eq!(
        headers.get_all("set-cookie").iter().count(),
        2,
        "both cookies go"
    );
    assert_eq!(body, b"hi");
    app.call("socket.close", json!({ "socket": served.id }))
        .await
        .unwrap();
}

#[tokio::test]
async fn requests_are_answered_in_any_order_and_a_body_comes_as_a_stream() {
    let app = app_for(&[]).await;
    let mut served = serve(&app, json!({})).await.unwrap();
    let upload: Vec<u8> = (0..3 * 1024 * 1024).map(big_byte).collect();
    let slow = tokio::spawn(fetch(
        "POST",
        served.url("/upload"),
        vec![("content-type", "application/octet-stream".to_owned())],
        upload.clone(),
    ));
    let first = next_request(&mut served.requests)
        .await
        .expect("the upload");
    let quick = plain_get(served.url("/quick"));
    let second = next_request(&mut served.requests)
        .await
        .expect("the quick one");
    assert_eq!(
        (first.method.as_str(), second.url.as_str()),
        ("POST", "/quick")
    );

    respond(&app, second.id, 200, &[], b"quick").await.unwrap();
    assert_eq!(
        quick.await.unwrap().unwrap().2,
        b"quick",
        "the second is answered first"
    );

    let mut body = Pipe::open(&app, first.body.expect("a body"));
    let got = body.exactly(upload.len()).await;
    assert!(got == upload, "the body came whole and in order");
    respond(&app, first.id, 200, &[], b"stored").await.unwrap();
    assert_eq!(slow.await.unwrap().unwrap().2, b"stored");
}

#[tokio::test]
async fn an_answer_can_be_a_stream_of_any_length() {
    let app = app_for(&[]).await;
    let mut served = serve(&app, json!({})).await.unwrap();
    let call = plain_get(served.url("/big"));
    let seen = next_request(&mut served.requests).await.unwrap();
    let started = app
        .call(
            "http.respondStream",
            json!({ "request": seen.id, "status": 200, "headers": [["content-type", "application/octet-stream"]] }),
        )
        .await
        .unwrap();
    let writer = app
        .session()
        .streams()
        .incoming_writer(StreamId(started["upload"].as_u64().unwrap()))
        .expect("an incoming stream");
    let data: Vec<u8> = (0..BIG).map(big_byte).collect();
    for piece in data.chunks(256 * 1024) {
        writer.write(Bytes::copy_from_slice(piece)).await.unwrap();
    }
    writer.end();
    let (status, headers, body) = call.await.unwrap().unwrap();
    assert_eq!(status, 200);
    assert_eq!(headers["content-type"], "application/octet-stream");
    assert!(body == data, "the answer came whole");
}

#[tokio::test]
async fn a_request_is_answered_once_and_only_with_what_an_answer_may_be() {
    let app = app_for(&[]).await;
    let mut served = serve(&app, json!({})).await.unwrap();
    let call = plain_get(served.url("/"));
    let seen = next_request(&mut served.requests).await.unwrap();
    for status in [0, 100, 199, 600, 999] {
        let refused = respond(&app, seen.id, status, &[], b"").await;
        assert_eq!(
            refused.unwrap_err().code,
            ErrorCode::InvalidArgument,
            "{status}"
        );
    }
    for header in [
        "content-length",
        "transfer-encoding",
        "connection",
        "upgrade",
        "host",
    ] {
        let refused = respond(&app, seen.id, 200, &[(header, "1")], b"").await;
        assert_eq!(
            refused.unwrap_err().code,
            ErrorCode::InvalidArgument,
            "{header}"
        );
    }
    respond(&app, seen.id, 200, &[], b"ok").await.unwrap();
    assert_eq!(
        call.await.unwrap().unwrap().2,
        b"ok",
        "a refusal leaves the request to be answered"
    );
    let again = respond(&app, seen.id, 200, &[], b"twice").await;
    assert_eq!(again.unwrap_err().code, ErrorCode::NotFound);
    let unknown = respond(&app, 424_242, 200, &[], b"").await;
    assert_eq!(unknown.unwrap_err().code, ErrorCode::NotFound);
    let not_a_request = respond(&app, served.id, 200, &[], b"").await;
    assert_eq!(
        not_a_request.unwrap_err().code,
        ErrorCode::NotFound,
        "a server is not a request"
    );
    let still = plain_get(served.url("/still"));
    let seen = next_request(&mut served.requests)
        .await
        .expect("the server was left as it was");
    respond(&app, seen.id, 200, &[], b"here").await.unwrap();
    assert_eq!(still.await.unwrap().unwrap().2, b"here");
    app.call("socket.close", json!({ "socket": served.id }))
        .await
        .expect("the server is still a resource that can be closed");
}

#[tokio::test]
async fn a_request_that_the_page_does_not_answer_gets_a_504_and_is_forgotten() {
    let app = app_for(&[]).await;
    let mut served = serve(&app, json!({ "answerTimeoutMs": 300 }))
        .await
        .unwrap();
    let started = std::time::Instant::now();
    let call = plain_get(served.url("/slow"));
    let seen = next_request(&mut served.requests).await.unwrap();
    let (status, _, _) = call.await.unwrap().unwrap();
    assert_eq!(status, StatusCode::GATEWAY_TIMEOUT);
    assert!(
        started.elapsed() < Duration::from_secs(10),
        "{:?}",
        started.elapsed()
    );
    let late = respond(&app, seen.id, 200, &[], b"late").await;
    assert_eq!(
        late.unwrap_err().code,
        ErrorCode::NotFound,
        "the request is gone"
    );
}

#[tokio::test]
async fn a_server_that_is_closed_takes_nobody_and_drops_the_requests_it_had() {
    let app = app_for(&[]).await;
    let mut served = serve(&app, json!({})).await.unwrap();
    let pending = plain_get(served.url("/pending"));
    let seen = next_request(&mut served.requests).await.unwrap();
    app.call("socket.close", json!({ "socket": served.id }))
        .await
        .unwrap();
    let outcome = tokio::time::timeout(Duration::from_secs(10), pending)
        .await
        .expect("the client of a closed server is told")
        .unwrap();
    assert!(outcome.is_err() || outcome.unwrap().0 != StatusCode::OK);
    let mut refused = false;
    for _ in 0..40 {
        if TcpStream::connect(("127.0.0.1", served.port))
            .await
            .is_err()
        {
            refused = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    assert!(refused, "a closed server still took a connection");
    // The request is forgotten with its connection.
    let mut forgotten = false;
    for _ in 0..100 {
        match respond(&app, seen.id, 200, &[], b"").await {
            Err(error) if error.code == ErrorCode::NotFound => {
                forgotten = true;
                break;
            }
            _ => tokio::time::sleep(Duration::from_millis(50)).await,
        }
    }
    assert!(forgotten, "a request outlived its server");
}

#[tokio::test]
async fn a_page_that_stops_taking_requests_leaves_the_clients_with_a_503() {
    let app = app_for(&[]).await;
    let mut served = serve(&app, json!({})).await.unwrap();
    let first = plain_get(served.url("/"));
    let seen = next_request(&mut served.requests).await.unwrap();
    respond(&app, seen.id, 200, &[], b"once").await.unwrap();
    assert_eq!(first.await.unwrap().unwrap().0, StatusCode::OK);
    app.session()
        .streams()
        .close(StreamId(served.reply["requests"].as_u64().unwrap()))
        .unwrap();
    let (status, _, body) = fetch("GET", served.url("/"), Vec::new(), Vec::new())
        .await
        .unwrap();
    assert_eq!(status, StatusCode::SERVICE_UNAVAILABLE);
    assert!(!body.is_empty(), "the client is told why");
}

#[tokio::test]
async fn a_request_whose_place_is_gone_is_told_to_the_client_as_a_500() {
    let app = app_for(&[]).await;
    let mut served = serve(&app, json!({})).await.unwrap();
    let call = plain_get(served.url("/"));
    let seen = next_request(&mut served.requests).await.unwrap();
    // What the end of a document does to the requests it had.
    app.session().resources().take(ResourceId(seen.id)).unwrap();
    let (status, _, _) = call.await.unwrap().unwrap();
    assert_eq!(status, StatusCode::INTERNAL_SERVER_ERROR);
}

#[tokio::test]
async fn a_port_is_taken_only_as_listen_lets_it_and_the_arguments_are_held() {
    let app = app_for(&[]).await;
    for args in [
        json!({ "host": "0.0.0.0", "port": 0 }),
        json!({ "host": "127.0.0.2", "port": 0 }),
    ] {
        let denied = app.call("http.serve", args.clone()).await;
        assert_eq!(
            denied.unwrap_err().code,
            ErrorCode::PermissionDenied,
            "{args}"
        );
    }
    let held = serve(&app, json!({})).await.unwrap();
    let taken = app
        .call("http.serve", json!({ "port": held.port }))
        .await
        .unwrap_err();
    assert_ne!(taken.code, ErrorCode::PermissionDenied, "{}", taken.message);
    let wrong_key = "-----BEGIN PRIVATE KEY-----\nAAAA\n-----END PRIVATE KEY-----\n";
    for bad in [
        json!({ "tls": { "cert": "nothing", "key": "nothing" } }),
        json!({ "tls": { "cert": tls::certificate() } }),
        json!({ "tls": { "cert": tls::certificate(), "key": wrong_key } }),
        json!({ "unknown": true }),
        json!({ "files": "/this/folder/is/not/here" }),
    ] {
        let mut args = bad.clone();
        args["port"] = json!(0);
        let refused = app.call("http.serve", args).await;
        assert!(refused.is_err(), "{bad}");
    }
    let none = Fixture::new(None, &[]).await;
    let denied = none.call("http.serve", json!({ "port": 0 })).await;
    assert_eq!(denied.unwrap_err().code, ErrorCode::PermissionDenied);
}

#[tokio::test]
async fn what_the_user_substituted_is_a_port_that_nobody_comes_to() {
    let mut consent = Consent::undecided();
    consent.set(Right::scoped("net.socket", LISTEN), Decision::Substitute);
    let app = app_for(&[]).await.with_consent(consent);
    let free = TcpListener::bind(("127.0.0.1", 0)).await.unwrap();
    let port = free.local_addr().unwrap().port();
    drop(free);
    let mut served = serve(&app, json!({ "port": port })).await.unwrap();
    assert_eq!(served.reply["address"]["host"], "127.0.0.1");
    assert_eq!(served.port, port, "the page is told the port it asked for");
    assert_eq!(served.reply["secure"], false);
    assert!(
        nothing_comes(&mut served.requests).await,
        "the stream waits and does not fail"
    );
    assert!(
        TcpStream::connect(("127.0.0.1", port)).await.is_err(),
        "something listens on a port the user substituted"
    );
    let secure = serve(
        &app,
        json!({ "port": port, "tls": { "cert": tls::certificate(), "key": tls::key() } }),
    )
    .await
    .unwrap();
    assert_eq!(secure.reply["secure"], true);
    app.call("socket.close", json!({ "socket": served.id }))
        .await
        .unwrap();
}

mod files;
mod held;
