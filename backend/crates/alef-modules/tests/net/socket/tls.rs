// SPDX-License-Identifier: MIT OR Apache-2.0
//! TLS of a connection, against a server of the test with a certificate of the test authority (the
//! files in `fixtures/tls` are made for these tests alone and protect nothing).
use super::*;
use crate::shared::tls::{acceptor, certificate, AUTHORITY};
use alef_core::security::consent::{Consent, Decision, Right};

/// A TLS server that gives back what it gets, and the number of the handshakes it finished.
async fn secure_echo(tls12_only: bool) -> (SocketAddr, Arc<AtomicUsize>) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let acceptor = acceptor(tls12_only);
    let done = Arc::new(AtomicUsize::new(0));
    let count = done.clone();
    tokio::spawn(async move {
        while let Ok((stream, _)) = listener.accept().await {
            let acceptor = acceptor.clone();
            let count = count.clone();
            tokio::spawn(async move {
                let Ok(mut stream) = acceptor.accept(stream).await else {
                    return;
                };
                count.fetch_add(1, Ordering::SeqCst);
                let (mut from, mut to) = tokio::io::split(&mut stream);
                let _ = tokio::io::copy(&mut from, &mut to).await;
                let _ = to.shutdown().await;
            });
        }
    });
    (address, done)
}

async fn secure_app(port: u16) -> Fixture {
    app(&[
        &format!("tcp:127.0.0.1:{port}"),
        &format!("tcp:localhost:{port}"),
    ])
    .await
}

#[tokio::test]
async fn a_connection_with_tls_carries_bytes_to_a_server_of_the_authority_the_page_names() {
    for tls12_only in [false, true] {
        let (server, done) = secure_echo(tls12_only).await;
        let app = secure_app(server.port()).await;
        let mut conn = connect(
            &app,
            "127.0.0.1",
            server.port(),
            json!({ "tls": { "ca": AUTHORITY } }),
        )
        .await
        .unwrap();
        conn.send(b"a secret").await;
        assert_eq!(
            conn.input.exactly(8).await,
            b"a secret",
            "tls12 only: {tls12_only}"
        );
        conn.finish();
        assert_eq!(conn.input.until_end().await.unwrap(), b"");
        assert_eq!(done.load(Ordering::SeqCst), 1);
    }
}

#[tokio::test]
async fn the_name_of_the_server_is_the_host_or_the_one_the_page_gives() {
    let (server, _) = secure_echo(false).await;
    let app = secure_app(server.port()).await;
    let by_name = connect(
        &app,
        "127.0.0.1",
        server.port(),
        json!({ "tls": { "ca": AUTHORITY, "serverName": "localhost" } }),
    )
    .await;
    assert!(by_name.is_ok(), "the certificate is for localhost too");
    let other = connect(
        &app,
        "127.0.0.1",
        server.port(),
        json!({ "tls": { "ca": AUTHORITY, "serverName": "other.test" } }),
    )
    .await
    .err()
    .expect("the certificate is not for that name");
    assert_eq!(other.code, ErrorCode::Network);
    assert!(
        other.message.starts_with("the TLS handshake failed"),
        "{}",
        other.message
    );
}

#[tokio::test]
async fn an_authority_nobody_trusts_fails_the_handshake_and_the_authorities_given_replace_the_roots(
) {
    let (server, _) = secure_echo(false).await;
    let app = secure_app(server.port()).await;
    // The roots of Mozilla do not know the authority of the test.
    let unknown = connect(&app, "127.0.0.1", server.port(), json!({ "tls": true }))
        .await
        .err()
        .unwrap();
    assert_eq!(unknown.code, ErrorCode::Network);
    assert!(
        unknown.message.starts_with("the TLS handshake failed"),
        "{}",
        unknown.message
    );
    // A certificate that is not the authority of the server does not make it trusted.
    let leaf = certificate();
    let wrong = connect(
        &app,
        "127.0.0.1",
        server.port(),
        json!({ "tls": { "ca": leaf } }),
    )
    .await
    .err()
    .unwrap();
    assert_eq!(wrong.code, ErrorCode::Network);
    for bad in [
        "",
        "not pem",
        "-----BEGIN CERTIFICATE-----\nAAAA\n-----END CERTIFICATE-----\n",
    ] {
        let refused = connect(
            &app,
            "127.0.0.1",
            server.port(),
            json!({ "tls": { "ca": bad } }),
        )
        .await;
        assert_eq!(code(refused), ErrorCode::InvalidArgument, "{bad:?}");
    }
    let unknown_option = connect(
        &app,
        "127.0.0.1",
        server.port(),
        json!({ "tls": { "verify": false } }),
    )
    .await;
    assert_eq!(code(unknown_option), ErrorCode::InvalidArgument);
}

#[tokio::test]
async fn a_handshake_that_never_ends_is_cut_by_the_timeout() {
    let silent = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = silent.local_addr().unwrap().port();
    tokio::spawn(async move {
        let mut held = Vec::new();
        while let Ok((stream, _)) = silent.accept().await {
            held.push(stream);
        }
    });
    let app = secure_app(port).await;
    let started = std::time::Instant::now();
    let stalled = connect(
        &app,
        "127.0.0.1",
        port,
        json!({ "tls": { "ca": AUTHORITY }, "timeoutMs": 300 }),
    )
    .await;
    assert_eq!(code(stalled), ErrorCode::Timeout);
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "{:?}",
        started.elapsed()
    );
}

#[tokio::test]
async fn a_peer_that_ends_tls_without_saying_so_is_a_peer_that_is_done() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    let acceptor = acceptor(false);
    tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        let mut stream = acceptor.accept(stream).await.unwrap();
        stream.write_all(b"last words").await.unwrap();
        stream.flush().await.unwrap();
        // Dropped here: the connection ends without a close_notify.
    });
    let app = secure_app(port).await;
    let mut conn = connect(
        &app,
        "127.0.0.1",
        port,
        json!({ "tls": { "ca": AUTHORITY } }),
    )
    .await
    .unwrap();
    assert_eq!(conn.input.exactly(10).await, b"last words");
    assert_eq!(conn.input.until_end().await.unwrap(), b"");
}

#[tokio::test]
async fn a_substituted_connection_with_tls_hangs_like_any_other() {
    let (server, done) = secure_echo(false).await;
    let scope = format!("tcp:127.0.0.1:{}", server.port());
    let mut consent = Consent::undecided();
    consent.set(Right::scoped("net.socket", &scope), Decision::Substitute);
    let app = crate::common::Fixture::new(Some(&manifest(&[&scope])), &[])
        .await
        .with_consent(consent);
    let dead = connect(
        &app,
        "127.0.0.1",
        server.port(),
        json!({ "tls": { "ca": AUTHORITY }, "timeoutMs": 200 }),
    )
    .await;
    assert_eq!(code(dead), ErrorCode::Timeout);
    assert_eq!(done.load(Ordering::SeqCst), 0);
}
