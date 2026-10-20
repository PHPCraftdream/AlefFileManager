// SPDX-License-Identifier: MIT OR Apache-2.0
//! Session cookie registry behavior over the local HTTP server.
use super::*;

#[tokio::test]
async fn jar_commands_store_send_list_clear_close_and_keep_session_ownership() {
    let server = Server::start().await;
    let app = app(&[server.scope()]).await;
    let jar = bounded(app.call("http.cookieJar", json!({})))
        .await
        .unwrap()["jar"]
        .as_u64()
        .unwrap();
    let head = bounded(ask(
        &app,
        json!({"url":server.url("/set-cookie"),"jar":jar}),
    ))
    .await
    .unwrap();
    body_of(&app, &head).await;
    let list = bounded(app.call(
        "http.cookieJar.list",
        json!({"jar":jar,"url":server.url("/")}),
    ))
    .await
    .unwrap();
    assert!(list
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v["name"] == "jar" && v["value"] == "value"));
    let head = bounded(ask(
        &app,
        json!({"url":server.url("/echo-cookie"),"jar":jar}),
    ))
    .await
    .unwrap();
    assert_eq!(header(&head, "x-cookie"), Some("jar=value"));
    body_of(&app, &head).await;
    bounded(app.call("http.cookieJar.clear", json!({"jar":jar})))
        .await
        .unwrap();
    assert!(bounded(app.call(
        "http.cookieJar.list",
        json!({"jar":jar,"url":server.url("/")})
    ))
    .await
    .unwrap()
    .as_array()
    .unwrap()
    .is_empty());
    let empty = bounded(ask(
        &app,
        json!({"url":server.url("/echo-cookie"),"jar":jar}),
    ))
    .await
    .unwrap();
    assert_eq!(header(&empty, "x-cookie"), Some(""));
    body_of(&app, &empty).await;
    bounded(app.call("http.cookieJar.close", json!({"jar":jar})))
        .await
        .unwrap();
    assert_eq!(
        code(
            bounded(app.call(
                "http.cookieJar.list",
                json!({"jar":jar,"url":server.url("/")})
            ))
            .await
        ),
        ErrorCode::NotFound
    );
    let second = bounded(app.call("http.cookieJar", json!({})))
        .await
        .unwrap()["jar"]
        .as_u64()
        .unwrap();
    bounded(ask(
        &app,
        json!({"url":server.url("/set-redirect-cookie"),"jar":second}),
    ))
    .await
    .unwrap();
    let list = bounded(app.call(
        "http.cookieJar.list",
        json!({"jar":second,"url":server.url("/")}),
    ))
    .await
    .unwrap();
    assert!(list
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v["name"] == "redirect"));
}

#[tokio::test]
async fn cookies_are_opt_in_cookie_conflicts_cover_all_transports_and_wrong_types_remain_open() {
    let server = Server::start().await;
    let app = app(&[server.scope()]).await;
    let no_jar_seed = bounded(ask(&app, json!({"url":server.url("/set-cookie")})))
        .await
        .unwrap();
    body_of(&app, &no_jar_seed).await;
    // Without a jar, the server's fixed fixture still returns an empty x-cookie header.
    let bare = bounded(ask(&app, json!({"url":server.url("/echo-cookie")})))
        .await
        .unwrap();
    assert_eq!(header(&bare, "x-cookie"), Some(""));
    body_of(&app, &bare).await;

    let jar = bounded(app.call("http.cookieJar", json!({})))
        .await
        .unwrap()["jar"]
        .as_u64()
        .unwrap();
    let seeded = bounded(ask(
        &app,
        json!({"url":server.url("/set-cookie"),"jar":jar}),
    ))
    .await
    .unwrap();
    body_of(&app, &seeded).await;
    let no_jar_echo = bounded(ask(&app, json!({"url":server.url("/echo-cookie")})))
        .await
        .unwrap();
    assert_eq!(header(&no_jar_echo, "x-cookie"), Some(""));
    body_of(&app, &no_jar_echo).await;

    let started = bounded(app.call(
        "http.start",
        json!({"url":server.url("/echo-cookie"),"method":"POST","jar":jar}),
    ))
    .await
    .unwrap();
    app.session()
        .streams()
        .incoming_writer(alef_core::ids::StreamId(
            started["upload"].as_u64().unwrap(),
        ))
        .expect("an incoming stream")
        .end();
    let streamed = bounded(app.call("http.response", json!({"request":started["request"]})))
        .await
        .unwrap();
    assert_eq!(header(&streamed, "x-cookie"), Some("jar=value"));
    body_of(&app, &streamed).await;

    for (command, args) in [
        (
            "http.request",
            json!({"url":server.url("/"),"jar":jar,"headers":[["Cookie","manual=1"]]}),
        ),
        (
            "http.start",
            json!({"url":server.url("/"),"jar":jar,"headers":[["Cookie","manual=1"]]}),
        ),
        (
            "http.download",
            json!({"url":server.url("/"),"path":"unused","jar":jar,"headers":[["Cookie","manual=1"]]}),
        ),
    ] {
        assert_eq!(
            bounded(app.call(command, args)).await.unwrap_err().code,
            ErrorCode::InvalidArgument
        );
    }
    let pending = bounded(app.call(
        "http.start",
        json!({"url":server.url("/slow"),"timeoutMs":10}),
    ))
    .await
    .unwrap();
    let wrong = bounded(app.call(
        "http.cookieJar.close",
        json!({"jar":pending["request"].as_u64().unwrap()}),
    ))
    .await
    .unwrap_err();
    assert_eq!(wrong.code, ErrorCode::NotFound);
    let response_error = bounded(app.call("http.response", json!({"request":pending["request"]})))
        .await
        .unwrap_err();
    assert_eq!(response_error.code, ErrorCode::Timeout);
}

#[tokio::test]
async fn redirect_uses_new_host_cookie_after_set_cookie_and_permission_prevents_denied_host_send() {
    let server = Server::start().await;
    let app = app(&[
        server.scope(),
        server.scope().replace("127.0.0.1", "localhost"),
    ])
    .await;
    let jar = bounded(app.call("http.cookieJar", json!({})))
        .await
        .unwrap()["jar"]
        .as_u64()
        .unwrap();
    // A cookie for 127.0.0.1 is host-only; localhost is independently authorized but does not inherit it.
    let set = bounded(ask(
        &app,
        json!({"url":server.url("/set-cookie"),"jar":jar}),
    ))
    .await
    .unwrap();
    body_of(&app, &set).await;
    let before = server.requests().len();
    let redirect_head = bounded(ask(
        &app,
        json!({"url":server.url("/redirect-cookie"),"jar":jar}),
    ))
    .await
    .unwrap();
    assert_eq!(header(&redirect_head, "x-cookie"), Some(""));
    body_of(&app, &redirect_head).await;
    let requests = server.requests();
    assert_eq!(
        requests[before + 1]
            .headers
            .get("cookie")
            .map(String::as_str)
            .unwrap_or(""),
        ""
    );
    assert_eq!(
        requests[before + 1].headers["host"],
        requests[before].headers["host"].replace("127.0.0.1", "localhost")
    );

    let other_set = bounded(ask(
        &app,
        json!({"url":server.url("/set-cookie?value=other").replace("127.0.0.1", "localhost"),"jar":jar}),
    ))
    .await
    .unwrap();
    body_of(&app, &other_set).await;
    let before = server.requests().len();
    let redirect_head = bounded(ask(
        &app,
        json!({"url":server.url("/redirect-cookie"),"jar":jar}),
    ))
    .await
    .unwrap();
    assert_eq!(header(&redirect_head, "x-cookie"), Some("jar=other"));
    body_of(&app, &redirect_head).await;
    let requests = server.requests();
    assert_eq!(
        requests[before + 1]
            .headers
            .get("cookie")
            .map(String::as_str),
        Some("jar=other")
    );

    let denied_app = super::app(&[server.scope()]).await;
    let other = Server::start().await;
    let j = bounded(denied_app.call("http.cookieJar", json!({})))
        .await
        .unwrap()["jar"]
        .as_u64()
        .unwrap();
    let allowed_seed = bounded(ask(
        &denied_app,
        json!({"url":server.url("/set-cookie"),"jar":j}),
    ))
    .await
    .unwrap();
    body_of(&denied_app, &allowed_seed).await;
    let allowed_list = bounded(denied_app.call(
        "http.cookieJar.list",
        json!({"jar":j,"url":server.url("/")}),
    ))
    .await
    .unwrap();
    assert!(allowed_list
        .as_array()
        .unwrap()
        .iter()
        .any(|v| v["name"] == "jar" && v["value"] == "value"));
    let error = bounded(ask(
        &denied_app,
        json!({"url":other.url("/echo-cookie"),"jar":j}),
    ))
    .await
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::PermissionDenied);
    assert!(other.requests().is_empty());
    let list =
        bounded(denied_app.call("http.cookieJar.list", json!({"jar":j,"url":other.url("/")})))
            .await
            .unwrap_err();
    assert_eq!(list.code, ErrorCode::PermissionDenied);
}

#[tokio::test]
async fn jars_are_isolated_between_sessions() {
    let server = Server::start().await;
    let a = app(&[server.scope()]).await;
    let b = app(&[server.scope()]).await;
    let jar = bounded(a.call("http.cookieJar", json!({}))).await.unwrap()["jar"]
        .as_u64()
        .unwrap();
    assert_eq!(
        bounded(b.call(
            "http.cookieJar.list",
            json!({"jar":jar,"url":server.url("/")})
        ))
        .await
        .unwrap_err()
        .code,
        ErrorCode::NotFound
    );
}

async fn bounded<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(10), future)
        .await
        .expect("cookie operation timed out")
}
