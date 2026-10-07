// SPDX-License-Identifier: MIT OR Apache-2.0
//! Redirects: followed by hand, every hop held against the scope again, the rules of Fetch for the method
//! and the body, and what the user substituted.
use super::*;

#[tokio::test]
async fn redirects_are_followed_by_hand_and_every_hop_is_held_against_the_scope() {
    let server = Server::start().await;
    let other = Server::start().await;
    let outside = Server::start().await;
    let app = app(&[server.scope(), other.scope()]).await;

    let followed = ask(
        &app,
        json!({ "url": server.url("/redirect/302?to=/hello") }),
    )
    .await
    .unwrap();
    assert_eq!(
        (followed["status"].as_u64(), followed["redirected"].clone()),
        (Some(200), json!(true))
    );
    assert_eq!(
        followed["url"],
        server.url("/hello"),
        "the address after the redirects"
    );
    assert_eq!(body_of(&app, &followed).await, b"hello");

    let manual = ask(
        &app,
        json!({ "url": server.url("/redirect/302?to=/hello"), "redirect": "manual" }),
    )
    .await
    .unwrap();
    assert_eq!(
        (manual["status"].as_u64(), manual["redirected"].clone()),
        (Some(302), json!(false))
    );
    assert_eq!(header(&manual, "location"), Some("/hello"));

    // 303 turns a POST into a GET without a body; 307 keeps the method and sends the body again.
    let mark = server.requests().len();
    let seen = ask_with(
        &app,
        json!({ "url": server.url("/redirect/303?to=/echo"), "method": "POST", "headers": [["content-type", "text/plain"]] }),
        b"payload",
    )
    .await
    .unwrap();
    assert_eq!(header(&seen, "x-method"), Some("GET"));
    assert!(body_of(&app, &seen).await.is_empty());
    assert_eq!(
        trail(&server, mark),
        ["POST /redirect/303 [payload]", "GET /echo []"]
    );
    let hops = server.requests();
    assert_eq!(hops[mark].headers["content-type"], "text/plain");
    assert!(
        !hops[mark + 1].headers.contains_key("content-type"),
        "the headers of a body went with it"
    );
    let mark = server.requests().len();
    let kept = ask_with(
        &app,
        json!({ "url": server.url("/redirect/307?to=/echo"), "method": "POST" }),
        b"payload",
    )
    .await
    .unwrap();
    assert_eq!(header(&kept, "x-method"), Some("POST"));
    assert_eq!(body_of(&app, &kept).await, b"payload");
    assert_eq!(
        trail(&server, mark),
        ["POST /redirect/307 [payload]", "POST /echo [payload]"]
    );

    // A 302 changes only a POST, a HEAD stays a HEAD, and a 300 is the answer itself.
    let mark = server.requests().len();
    let put = ask_with(
        &app,
        json!({ "url": server.url("/redirect/302?to=/echo"), "method": "PUT" }),
        b"payload",
    )
    .await
    .unwrap();
    assert_eq!(body_of(&app, &put).await, b"payload");
    let head = ask(
        &app,
        json!({ "url": server.url("/redirect/302?to=/hello"), "method": "HEAD" }),
    )
    .await
    .unwrap();
    assert_eq!(
        (head["status"].as_u64(), head["stream"].clone()),
        (Some(200), Value::Null)
    );
    let choice = ask(
        &app,
        json!({ "url": server.url("/redirect/300?to=/hello") }),
    )
    .await
    .unwrap();
    body_of(&app, &choice).await;
    assert_eq!(
        (choice["status"].as_u64(), choice["redirected"].clone()),
        (Some(300), json!(false))
    );
    assert_eq!(
        trail(&server, mark),
        [
            "PUT /redirect/302 [payload]",
            "PUT /echo [payload]",
            "HEAD /redirect/302 []",
            "HEAD /hello []",
            "GET /redirect/300 []"
        ]
    );

    // To another origin the credentials do not go along.
    let across = other.url("/hello");
    let moved = ask(
        &app,
        json!({ "url": server.url(&format!("/redirect/302?to={across}")), "headers": [["authorization", "secret"], ["cookie", "a=b"], ["x-keep", "1"]] }),
    )
    .await
    .unwrap();
    assert_eq!(moved["url"], across);
    body_of(&app, &moved).await;
    let arrived = other.requests().pop().unwrap();
    assert!(
        !arrived.headers.contains_key("authorization") && !arrived.headers.contains_key("cookie"),
        "{:?}",
        arrived.headers
    );
    assert_eq!(arrived.headers["x-keep"], "1");
    let same = ask(&app, json!({ "url": server.url("/redirect/302?to=/hello"), "headers": [["authorization", "secret"]] })).await.unwrap();
    body_of(&app, &same).await;
    assert_eq!(
        server.requests().pop().unwrap().headers["authorization"],
        "secret",
        "inside one origin they stay"
    );

    // A redirect to where the manifest does not reach is a denial, and nothing is sent there.
    let away = outside.url("/hello");
    let denied = ask(
        &app,
        json!({ "url": server.url(&format!("/redirect/302?to={away}")) }),
    )
    .await
    .unwrap_err();
    assert_eq!(denied.code, ErrorCode::PermissionDenied);
    assert!(outside.requests().is_empty());
    let mark = server.requests().len();
    let loops = ask(
        &app,
        json!({ "url": server.url("/loop"), "timeoutMs": 20000 }),
    )
    .await;
    assert_eq!(code(loops), ErrorCode::Network, "too many redirects");
    assert_eq!(
        server.requests().len() - mark,
        11,
        "the first request and ten redirects"
    );
}

#[tokio::test]
async fn a_redirect_to_an_address_the_user_substituted_is_not_followed() {
    let server = Server::start().await;
    let other = Server::start().await;
    let mut consent = Consent::undecided();
    consent.set(Right::scoped("net.http", &server.scope()), Decision::Allow);
    consent.set(
        Right::scoped("net.http", &other.scope()),
        Decision::Substitute,
    );
    let app = Fixture::new(Some(&manifest(&[server.scope(), other.scope()], &[])), &[])
        .await
        .with_consent(consent);
    let away = other.url("/hello");
    let error = ask(
        &app,
        json!({ "url": server.url(&format!("/redirect/302?to={away}")) }),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::PermissionDenied);
    assert!(
        other.requests().is_empty(),
        "the stand-in of the user is no server to ask"
    );
}

#[tokio::test]
async fn a_request_that_streams_its_body_follows_the_redirect_option_too() {
    let server = Server::start().await;
    let app = app(&[server.scope()]).await;
    for (redirect, status, redirected) in [("manual", 303, false), ("follow", 200, true)] {
        let started = app
            .call(
                "http.start",
                json!({ "url": server.url("/redirect/303?to=/hello"), "method": "POST", "redirect": redirect }),
            )
            .await
            .unwrap();
        app.session()
            .streams()
            .incoming_writer(alef_core::ids::StreamId(
                started["upload"].as_u64().unwrap(),
            ))
            .expect("an incoming stream")
            .end();
        let head = app
            .call("http.response", json!({ "request": started["request"] }))
            .await
            .unwrap();
        assert_eq!(
            (head["status"].as_u64(), head["redirected"].clone()),
            (Some(status), json!(redirected)),
            "{redirect}"
        );
        body_of(&app, &head).await;
    }
}
