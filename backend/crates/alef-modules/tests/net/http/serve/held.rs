// SPDX-License-Identifier: MIT OR Apache-2.0
//! What the server holds against the clients: the Host and the Origin, and TLS.
use super::*;

#[tokio::test]
async fn the_host_and_the_origin_of_a_request_are_held_against_the_server() {
    let app = app_for(&[]).await;
    let mut served = serve(
        &app,
        json!({ "origins": ["https://App.Test"], "hosts": ["App.Test"] }),
    )
    .await
    .unwrap();
    let port = served.port;
    for (headers, expected) in [
        (
            vec![("host", "evil.test".to_owned())],
            StatusCode::MISDIRECTED_REQUEST,
        ),
        (
            vec![("host", format!("127.0.0.1:{}", port + 1))],
            StatusCode::MISDIRECTED_REQUEST,
        ),
        (
            vec![("origin", "http://evil.test".to_owned())],
            StatusCode::FORBIDDEN,
        ),
        (
            vec![("origin", format!("http://evil.test:{port}"))],
            StatusCode::FORBIDDEN,
        ),
        (vec![("origin", "null".to_owned())], StatusCode::FORBIDDEN),
        (
            vec![("origin", format!("http://127.0.0.1:{}", port + 1))],
            StatusCode::FORBIDDEN,
        ),
    ] {
        let reply = fetch("GET", served.url("/"), headers.clone(), Vec::new())
            .await
            .unwrap();
        assert_eq!(reply.0, expected, "{headers:?}");
        assert!(
            nothing_comes(&mut served.requests).await,
            "{headers:?} reached the page"
        );
    }
    for origin in [
        format!("http://127.0.0.1:{port}"),
        format!("http://localhost:{port}"),
        "https://app.test".to_owned(),
    ] {
        let call = tokio::spawn(fetch(
            "GET",
            served.url("/"),
            vec![
                ("origin", origin.clone()),
                ("host", format!("localhost:{port}")),
            ],
            Vec::new(),
        ));
        let seen = next_request(&mut served.requests).await.expect(&origin);
        respond(&app, seen.id, 200, &[], b"in").await.unwrap();
        assert_eq!(call.await.unwrap().unwrap().0, StatusCode::OK, "{origin}");
    }
    for host in [
        format!("app.test:{port}"),
        "app.test".to_owned(),
        format!("APP.test:{port}"),
    ] {
        let call = tokio::spawn(fetch(
            "GET",
            served.url("/"),
            vec![("host", host.clone())],
            Vec::new(),
        ));
        let seen = next_request(&mut served.requests).await.expect(&host);
        respond(&app, seen.id, 200, &[], b"named").await.unwrap();
        assert_eq!(call.await.unwrap().unwrap().0, StatusCode::OK, "{host}");
    }
    let call = plain_get(served.url("/"));
    let seen = next_request(&mut served.requests).await.unwrap();
    assert!(
        seen.all("origin").is_empty(),
        "a client without an origin is let in"
    );
    respond(&app, seen.id, 204, &[], b"").await.unwrap();
    assert_eq!(call.await.unwrap().unwrap().0, StatusCode::NO_CONTENT);
}

/// A client of HTTP over TLS that trusts the authority of the tests.
async fn fetch_secure(port: u16, path: &str) -> Result<Reply, String> {
    let secure = secure_connect(port).await?;
    let (mut sender, connection) = hyper::client::conn::http1::handshake(TokioIo::new(secure))
        .await
        .map_err(|e| e.to_string())?;
    tokio::spawn(connection);
    let request = Request::builder()
        .uri(path)
        .header("host", format!("127.0.0.1:{port}"))
        .body(Full::new(Bytes::new()))
        .map_err(|e| e.to_string())?;
    let response = sender
        .send_request(request)
        .await
        .map_err(|e| e.to_string())?;
    let (parts, body) = response.into_parts();
    let bytes = body.collect().await.map_err(|e| e.to_string())?.to_bytes();
    Ok((parts.status, parts.headers, bytes.to_vec()))
}

#[tokio::test]
async fn a_server_with_tls_speaks_it_and_only_it() {
    let app = app_for(&[]).await;
    let mut served = serve(
        &app,
        json!({ "tls": { "cert": tls::certificate(), "key": tls::key() } }),
    )
    .await
    .unwrap();
    assert_eq!(served.reply["secure"], true);
    let port = served.port;
    let call = tokio::spawn(async move { fetch_secure(port, "/secure").await });
    let seen = next_request(&mut served.requests)
        .await
        .expect("a request over TLS");
    assert_eq!(seen.url, "/secure");
    respond(&app, seen.id, 200, &[], b"private").await.unwrap();
    assert_eq!(call.await.unwrap().unwrap().2, b"private");
    let plain = fetch("GET", served.url("/"), Vec::new(), Vec::new()).await;
    assert!(
        plain.is_err(),
        "a plain client does not get through to a server of TLS: {plain:?}"
    );
    assert!(nothing_comes(&mut served.requests).await);
}
