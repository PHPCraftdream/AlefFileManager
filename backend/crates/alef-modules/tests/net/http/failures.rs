// SPDX-License-Identifier: MIT OR Apache-2.0
//! What fails: a request that is closed, a body that is cut, a connection that breaks, an upload the page
//! breaks.
use super::*;

#[tokio::test]
async fn a_request_that_is_closed_before_its_answer_is_asked_for_cuts_the_connection() {
    let server = Server::start().await;
    let app = app(&[server.scope()]).await;
    let started = app
        .call("http.start", json!({ "url": server.url("/slow") }))
        .await
        .unwrap();
    app.session()
        .streams()
        .incoming_writer(alef_core::ids::StreamId(
            started["upload"].as_u64().unwrap(),
        ))
        .expect("an incoming stream")
        .end();
    assert!(
        until(|| server.requests().len() == 1).await,
        "the request did not arrive"
    );
    let id = alef_core::ids::ResourceId(started["request"].as_u64().unwrap());
    app.session().resources().take(id).unwrap().close().await;
    assert!(
        until(|| server.cut().contains(&"/slow".to_owned())).await,
        "a request nobody waits for was left on the connection"
    );
}

#[tokio::test]
async fn a_connection_that_breaks_while_the_body_comes_fails_the_stream_of_the_body() {
    let server = Server::start().await;
    let app = app(&[server.scope()]).await;
    let head = ask(&app, json!({ "url": server.url("/cut") }))
        .await
        .unwrap();
    let ended = body_result(&app, &head).await.unwrap_err();
    assert_eq!(ended.code, ErrorCode::Network);
}

#[tokio::test]
async fn a_failure_of_the_connection_is_the_network_and_says_what_failed() {
    let closed = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let hangup = Server::hangup().await.port();
    let app = app(&[
        format!("http://127.0.0.1:{closed}/*"),
        format!("http://127.0.0.1:{hangup}/*"),
    ])
    .await;
    let nobody = ask(
        &app,
        json!({ "url": format!("http://127.0.0.1:{closed}/") }),
    )
    .await
    .unwrap_err();
    assert_eq!(nobody.code, ErrorCode::Network);
    assert_eq!(nobody.message, "cannot connect to the server");
    let hung_up = ask(
        &app,
        json!({ "url": format!("http://127.0.0.1:{hangup}/") }),
    )
    .await
    .unwrap_err();
    assert_eq!(hung_up.code, ErrorCode::Network);
    assert_eq!(hung_up.message, "the request failed");
}

#[tokio::test]
async fn a_stream_the_page_breaks_while_it_writes_fails_the_request() {
    let server = Server::start().await;
    let app = app(&[server.scope()]).await;
    let started = app
        .call(
            "http.start",
            json!({ "url": server.url("/sum"), "method": "POST" }),
        )
        .await
        .unwrap();
    let writer = app
        .session()
        .streams()
        .incoming_writer(alef_core::ids::StreamId(
            started["upload"].as_u64().unwrap(),
        ))
        .expect("an incoming stream");
    writer.write(Bytes::from_static(b"part")).await.unwrap();
    writer.abort(AlefError::new(ErrorCode::Closed, "the page went away"));
    let answer = app
        .call("http.response", json!({ "request": started["request"] }))
        .await;
    assert_eq!(code(answer), ErrorCode::Network);
}
