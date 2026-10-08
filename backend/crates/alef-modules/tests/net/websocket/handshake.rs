// SPDX-License-Identifier: MIT OR Apache-2.0
//! The handshake and what it is held to: the headers and the subprotocols, the rights, the failures of
//! the network, TLS, the stand-in of the user, what each command takes.
use alef_core::security::consent::{Consent, Decision, Right};

use super::*;
use crate::shared::{
    manifest::{app, manifest},
    tls::{acceptor, AUTHORITY},
};

#[tokio::test]
async fn the_headers_and_the_subprotocols_of_the_page_reach_the_server_and_the_answer_comes_back() {
    let server = WsServer::start().await;
    let app = app(&[server.scope()], &[]).await;
    let open = connect(
        &app,
        &server.url("/echo?x=1"),
        json!({
            "headers": [["X-One", "1"], ["Origin", "https://app.test"], ["x-two", "2"], ["x-two", "3"], ["cookie", "a=1"], ["cookie", "b=2"]],
            "protocols": ["chat", "superchat"],
        }),
    )
    .await
    .unwrap();
    assert_eq!(open.protocol, "superchat", "the one the server chose");
    assert_eq!(open.url, server.url("/echo?x=1"));
    let visit = &server.visits()[0];
    assert_eq!(visit.path, "/echo");
    assert_eq!(visit.headers["x-one"], "1");
    assert_eq!(visit.headers["origin"], "https://app.test");
    assert_eq!(
        visit.headers["x-two"], "2, 3",
        "both values reach the server"
    );
    assert_eq!(
        visit.headers["cookie"], "a=1; b=2",
        "the cookies are joined as cookies are"
    );
    assert_eq!(visit.headers["sec-websocket-protocol"], "chat, superchat");
    assert!(
        visit.headers["user-agent"].starts_with("Alef/"),
        "{:?}",
        visit.headers
    );

    let own = connect(
        &app,
        &server.url("/echo"),
        json!({ "headers": [["user-agent", "mine"]] }),
    )
    .await
    .unwrap();
    assert_eq!(server.visits()[1].headers["user-agent"], "mine");

    let before = server.visits().len();
    for refused in [
        json!({ "headers": [["Host", "x"]] }),
        json!({ "headers": [["Sec-WebSocket-Key", "x"]] }),
        json!({ "headers": [["sec-websocket-protocol", "x"]] }),
        json!({ "headers": [["Upgrade", "x"]] }),
        json!({ "protocols": ["a b"] }),
        json!({ "protocols": ["chat", "chat"] }),
        json!({ "protocols": [""] }),
        json!({ "ca": "-----BEGIN CERTIFICATE-----" }),
        json!({ "verify": false }),
    ] {
        let result = connect(&app, &server.url("/echo"), refused.clone()).await;
        assert_eq!(code(result), ErrorCode::InvalidArgument, "{refused}");
    }
    assert_eq!(server.visits().len(), before, "nothing refused was sent");
    close(&app, open.id, None, "").await.unwrap();
    close(&app, own.id, None, "").await.unwrap();
}

#[tokio::test]
async fn an_address_outside_the_scope_is_denied_and_the_scheme_is_ws_or_wss() {
    let server = WsServer::start().await;
    let other = WsServer::start().await;
    let http = format!("http://127.0.0.1:{}/*", server.address.port());
    let app = app(&[server.scope(), http], &[]).await;
    for url in [
        other.url("/echo"),
        server.url("/echo").replace("127.0.0.1", "localhost"),
        server.secure_url("/echo"),
        "ws://example.com/".to_owned(),
    ] {
        let error = connect(&app, &url, json!({})).await.err().unwrap();
        assert_eq!(error.code, ErrorCode::PermissionDenied, "{url}");
        assert_eq!(error.details, Some(json!({ "permission": "net.http" })));
    }
    assert!(server.visits().is_empty() && other.visits().is_empty());
    let scheme = connect(
        &app,
        &format!("http://127.0.0.1:{}/echo", server.address.port()),
        json!({}),
    )
    .await;
    assert_eq!(
        code(scheme),
        ErrorCode::InvalidArgument,
        "a right of http is no right to speak the protocol of the other scheme"
    );
    let none = Fixture::new(None, &[]).await;
    assert_eq!(
        code(connect(&none, &server.url("/echo"), json!({})).await),
        ErrorCode::PermissionDenied
    );
}

#[tokio::test]
async fn a_server_that_does_not_upgrade_or_is_not_there_is_the_network() {
    let page = canned("HTTP/1.1 200 OK\r\ncontent-length: 2\r\n\r\nhi").await;
    let moved = canned(
        "HTTP/1.1 301 Moved Permanently\r\nlocation: ws://127.0.0.1:1/\r\ncontent-length: 0\r\n\r\n",
    )
    .await;
    let gone = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let scopes: Vec<String> = [page.port(), moved.port(), gone]
        .iter()
        .map(|port| format!("ws://127.0.0.1:{port}/*"))
        .collect();
    let app = app(&scopes, &[]).await;
    let plain = connect(&app, &format!("ws://127.0.0.1:{}/", page.port()), json!({}))
        .await
        .err()
        .unwrap();
    assert_eq!(plain.code, ErrorCode::Network);
    assert_eq!(
        plain.message,
        "the server did not upgrade the connection: it answered 200"
    );
    let redirected = connect(
        &app,
        &format!("ws://127.0.0.1:{}/", moved.port()),
        json!({}),
    )
    .await
    .err()
    .unwrap();
    assert_eq!(redirected.code, ErrorCode::Network);
    assert_eq!(
        redirected.message, "the server did not upgrade the connection: it answered 301",
        "a redirect of the handshake is followed by nobody"
    );
    let nobody = connect(&app, &format!("ws://127.0.0.1:{gone}/"), json!({}))
        .await
        .err()
        .unwrap();
    assert_eq!(nobody.code, ErrorCode::Network);
    assert!(
        nobody.message.starts_with("cannot connect to the server"),
        "{}",
        nobody.message
    );
}

#[tokio::test]
async fn a_handshake_that_never_ends_is_cut_by_the_timeout() {
    let address = silent().await;
    let scope = format!("ws://127.0.0.1:{}/*", address.port());
    let app = app(&[scope], &[]).await;
    let started = std::time::Instant::now();
    let stalled = connect(
        &app,
        &format!("ws://127.0.0.1:{}/", address.port()),
        json!({ "timeoutMs": 300 }),
    )
    .await;
    assert_eq!(code(stalled), ErrorCode::Timeout);
    assert!(
        started.elapsed() < std::time::Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn wss_trusts_the_authority_the_page_names_and_no_other() {
    let server = WsServer::start_secure(acceptor(false)).await;
    let app = app(&[server.secure_scope()], &[]).await;
    let mut open = connect(
        &app,
        &server.secure_url("/echo"),
        json!({ "ca": AUTHORITY }),
    )
    .await
    .unwrap();
    send(&app, open.id, true, b"secure").await.unwrap();
    assert_eq!(
        event(&mut open.messages).await,
        Some(Event::Text("secure".to_owned()))
    );
    close(&app, open.id, None, "").await.unwrap();

    let unknown = connect(&app, &server.secure_url("/echo"), json!({}))
        .await
        .err()
        .unwrap();
    assert_eq!(unknown.code, ErrorCode::Network);
    assert!(
        unknown.message.starts_with("the TLS handshake failed"),
        "{}",
        unknown.message
    );
    let wrong = connect(
        &app,
        &server.secure_url("/echo"),
        json!({ "ca": crate::shared::tls::certificate() }),
    )
    .await;
    assert_eq!(code(wrong), ErrorCode::Network);
    let not_pem = connect(
        &app,
        &server.secure_url("/echo"),
        json!({ "ca": "nothing" }),
    )
    .await;
    assert_eq!(code(not_pem), ErrorCode::InvalidArgument);
    assert_eq!(
        server.visits().len(),
        1,
        "only the handshake of the trusted authority got through"
    );
}

#[tokio::test]
async fn what_the_user_substituted_is_a_dead_network() {
    let server = WsServer::start().await;
    let mut consent = Consent::undecided();
    consent.set(
        Right::scoped("net.http", &server.scope()),
        Decision::Substitute,
    );
    let app = Fixture::new(Some(&manifest(&[server.scope()], &[])), &[])
        .await
        .with_consent(consent);
    let started = std::time::Instant::now();
    let dead = connect(&app, &server.url("/echo"), json!({ "timeoutMs": 300 })).await;
    assert_eq!(code(dead), ErrorCode::Timeout);
    assert!(started.elapsed() >= std::time::Duration::from_millis(250));
    assert!(started.elapsed() < std::time::Duration::from_secs(5));
    assert!(server.visits().is_empty(), "nothing reached the server");
}

#[tokio::test]
async fn the_commands_take_only_what_is_theirs() {
    let server = WsServer::start().await;
    let tcp = format!("tcp:127.0.0.1:{}", server.address.port());
    let app = app(&[server.scope()], &[&tcp]).await;
    let open = connect(&app, &server.url("/echo"), json!({}))
        .await
        .unwrap();
    let plain = app
        .call(
            "socket.connect",
            json!({ "host": "127.0.0.1", "port": server.address.port() }),
        )
        .await
        .unwrap();
    let tcp_id = plain["socket"].as_u64().unwrap();
    assert_eq!(
        code(send(&app, tcp_id, true, b"x").await),
        ErrorCode::NotFound,
        "a connection of TCP is no WebSocket"
    );
    assert_eq!(
        code(close(&app, tcp_id, None, "").await),
        ErrorCode::NotFound
    );
    assert_eq!(
        code(app.call("socket.close", json!({ "socket": open.id })).await),
        ErrorCode::NotFound,
        "and a WebSocket is no connection of TCP"
    );
    assert_eq!(
        code(send(&app, 4242, true, b"x").await),
        ErrorCode::NotFound
    );
    let unknown = app
        .call(
            "websocket.send",
            json!({ "socket": open.id, "binary": true }),
        )
        .await;
    assert_eq!(code(unknown), ErrorCode::InvalidArgument);
    close(&app, open.id, None, "").await.unwrap();
    app.call("socket.close", json!({ "socket": tcp_id }))
        .await
        .unwrap();
}

#[tokio::test]
async fn a_wrong_answer_of_the_handshake_fails_it() {
    let wrong = canned(
        "HTTP/1.1 101 Switching Protocols\r\nupgrade: websocket\r\nconnection: upgrade\r\nsec-websocket-accept: nonsense\r\n\r\n",
    )
    .await;
    let scope = format!("ws://127.0.0.1:{}/*", wrong.port());
    let app = app(&[scope], &[]).await;
    let error = connect(
        &app,
        &format!("ws://127.0.0.1:{}/", wrong.port()),
        json!({}),
    )
    .await
    .err()
    .unwrap();
    assert_eq!(error.code, ErrorCode::Network);
    assert!(
        error.message.starts_with("the handshake failed"),
        "{}",
        error.message
    );
}

#[tokio::test]
async fn a_connection_goes_with_its_resource_when_the_session_lets_it_go() {
    use alef_core::ids::ResourceId;
    let server = WsServer::start().await;
    let app = app(&[server.scope()], &[]).await;
    let open = connect(&app, &server.url("/echo"), json!({}))
        .await
        .unwrap();
    app.session()
        .resources()
        .take(ResourceId(open.id))
        .unwrap()
        .close()
        .await;
    assert!(
        until(|| server.visits()[0].ended).await,
        "the connection stayed open after its resource was closed"
    );
}
