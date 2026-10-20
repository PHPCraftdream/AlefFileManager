// SPDX-License-Identifier: MIT OR Apache-2.0
use std::time::Duration;

use super::*;
use crate::net::http::body::RequestBody;
use crate::net::http::proxy::transport::HEAD_LIMIT;
use bytes::Bytes;
use http_body_util::{BodyExt, Empty};
use hyper::{Method, Request};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::TcpListener,
};

fn empty_request(url: &Url) -> Request<RequestBody> {
    Request::builder()
        .method(Method::GET)
        .uri(url.as_str())
        .body(
            Empty::<Bytes>::new()
                .map_err(|never| match never {})
                .boxed(),
        )
        .unwrap()
}
fn proxy(addr: std::net::SocketAddr) -> Proxy {
    Proxy {
        url: Url::parse(&format!("http://{addr}")).unwrap(),
        address: addr.ip().to_string(),
        port: addr.port(),
    }
}
fn deadline() -> tokio::time::Instant {
    tokio::time::Instant::now() + Duration::from_secs(2)
}

#[test]
fn parser_accepts_dns_default_port_and_ipv6() {
    for (input, host, port) in [
        ("http://proxy.example:8080", "proxy.example", 8080),
        ("http://proxy.example:80", "proxy.example", 80),
        ("http://[::1]:8080", "::1", 8080),
    ] {
        let parsed = parse(Some(input)).unwrap().unwrap();
        assert_eq!(parsed.address, host);
        assert_eq!(parsed.port, port);
    }
}
#[test]
fn parser_rejects_bad_authorities_with_invalid_argument() {
    for input in [
        "http://host",
        "http://host:",
        "http://host:abc",
        "http://host:0",
        "http://@host:80",
        "http://host\\@evil:80",
        "http://host :80",
        "http://host:80/path",
        "https://host:80",
        "http://[::1]",
        "socks5://host:1080",
        "http://user:password@host:80",
        "http://:80",
    ] {
        assert_eq!(
            parse(Some(input)).unwrap_err().code,
            alef_core::ErrorCode::InvalidArgument,
            "{input}"
        );
    }
}

#[tokio::test]
async fn absolute_form_host_and_fragment_removal() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let p = proxy(listener.local_addr().unwrap());
    let peer = tokio::spawn(async move {
        let (mut s, _) = tokio::time::timeout(Duration::from_secs(10), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let mut bytes = Vec::new();
        while !bytes.windows(4).any(|w| w == b"\r\n\r\n") {
            let mut b = [0];
            tokio::time::timeout(Duration::from_secs(10), s.read_exact(&mut b))
                .await
                .unwrap()
                .unwrap();
            bytes.push(b[0]);
        }
        let req = String::from_utf8(bytes).unwrap();
        s.write_all(b"HTTP/1.1 204 No Content\r\nContent-Length: 0\r\n\r\n")
            .await
            .unwrap();
        req
    });
    let url = Url::parse("http://no-dns.invalid/path?q=1#frag").unwrap();
    let response = request(&p, &url, empty_request(&url), deadline())
        .await
        .unwrap();
    assert_eq!(response.status(), 204);
    let seen = peer.await.unwrap();
    assert!(
        seen.starts_with("GET http://no-dns.invalid/path?q=1 HTTP/1.1\r\n"),
        "{seen}"
    );
    assert!(
        seen.to_ascii_lowercase()
            .contains("host: no-dns.invalid:80\r\n"),
        "{seen}"
    );
}

#[tokio::test]
async fn non_success_connect_status_is_network_error() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let p = proxy(listener.local_addr().unwrap());
    let peer = tokio::spawn(async move {
        let (mut s, _) = tokio::time::timeout(Duration::from_secs(10), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let mut bytes = Vec::new();
        while !bytes.ends_with(b"\r\n\r\n") {
            assert!(bytes.len() < 2048);
            let mut b = [0];
            tokio::time::timeout(Duration::from_secs(10), s.read_exact(&mut b))
                .await
                .unwrap()
                .unwrap();
            bytes.push(b[0]);
        }
        assert!(bytes.starts_with(b"CONNECT example.invalid:443 HTTP/1.1\r\n"));
        assert!(bytes.ends_with(b"\r\n\r\n"));
        s.write_all(b"HTTP/1.1 407 Proxy Authentication Required\r\nContent-Length: 0\r\n\r\n")
            .await
            .unwrap();
        let mut extra = [0];
        // The client closes at once after the refusal; the bound only matters when it is broken.
        match tokio::time::timeout(Duration::from_secs(5), s.read(&mut extra)).await {
            Ok(Ok(0)) => {}
            Ok(Err(error)) if error.kind() == std::io::ErrorKind::ConnectionReset => {}
            Ok(Ok(count)) => panic!("unexpected post-407 bytes: {count}"),
            Ok(Err(error)) => panic!("unexpected socket error after 407: {error}"),
            Err(_) => panic!("proxy socket remained open after 407"),
        }
    });
    let url = Url::parse("https://example.invalid/").unwrap();
    let error = request(&p, &url, empty_request(&url), deadline())
        .await
        .err()
        .expect("CONNECT must be refused");
    assert_eq!(error.code, alef_core::ErrorCode::Network);
    assert_eq!(error.message, "the proxy refused the CONNECT tunnel");
    peer.await.unwrap();
}
#[tokio::test]
async fn oversized_connect_head_is_rejected() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let p = proxy(listener.local_addr().unwrap());
    let peer = tokio::spawn(async move {
        let (mut s, _) = tokio::time::timeout(Duration::from_secs(10), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let mut bytes = Vec::new();
        while !bytes.ends_with(b"\r\n\r\n") {
            assert!(bytes.len() < 2048);
            let mut b = [0];
            tokio::time::timeout(Duration::from_secs(10), s.read_exact(&mut b))
                .await
                .unwrap()
                .unwrap();
            bytes.push(b[0]);
        }
        s.write_all(b"HTTP/1.1 200 Connection established\r\n")
            .await
            .unwrap();
        s.write_all(&vec![b'a'; HEAD_LIMIT - 32]).await.unwrap();
        let mut extra = [0];
        tokio::time::timeout(Duration::from_secs(10), s.read(&mut extra))
            .await
            .unwrap()
            .unwrap()
    });
    let url = Url::parse("https://example.invalid/").unwrap();
    let response = request(
        &p,
        &url,
        empty_request(&url),
        tokio::time::Instant::now() + Duration::from_secs(10),
    )
    .await;
    assert_eq!(response.err().unwrap().code, alef_core::ErrorCode::Network);
    assert_eq!(peer.await.unwrap(), 0);
}
#[tokio::test]
async fn slow_connect_response_has_absolute_deadline_and_closes_socket() {
    struct AbortOnDrop(Option<tokio::task::JoinHandle<std::io::Result<usize>>>);
    impl Drop for AbortOnDrop {
        fn drop(&mut self) {
            if let Some(handle) = &self.0 {
                handle.abort();
            }
        }
    }

    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let p = proxy(listener.local_addr().unwrap());
    let mut peer = AbortOnDrop(Some(tokio::spawn(async move {
        let (mut s, _) = tokio::time::timeout(Duration::from_secs(10), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let mut bytes = Vec::new();
        while !bytes.ends_with(b"\r\n\r\n") {
            assert!(bytes.len() < 2048);
            let mut b = [0];
            tokio::time::timeout(Duration::from_secs(10), s.read_exact(&mut b))
                .await
                .unwrap()
                .unwrap();
            bytes.push(b[0]);
        }
        assert!(bytes.starts_with(b"CONNECT example.invalid:443 HTTP/1.1\r\n"));
        s.write_all(b"HTTP/1.1 200 OK\r\nX-Slow: ").await?;
        let mut byte = [0];
        match tokio::time::timeout(Duration::from_secs(10), s.read(&mut byte)).await {
            Ok(Ok(n)) => Ok(n),
            Ok(Err(error)) if error.kind() == std::io::ErrorKind::ConnectionReset => Ok(0),
            Ok(Err(error)) => Err(error),
            Err(_) => panic!("timed out waiting for proxy socket close"),
        }
    })));
    let url = Url::parse("https://example.invalid/").unwrap();
    let result = tokio::time::timeout(
        Duration::from_secs(10),
        request(
            &p,
            &url,
            empty_request(&url),
            tokio::time::Instant::now() + Duration::from_millis(100),
        ),
    )
    .await
    .expect("request exceeded outer test bound");
    assert_eq!(result.err().unwrap().code, alef_core::ErrorCode::Timeout);
    let handle = peer.0.take().unwrap();
    let closed = tokio::time::timeout(Duration::from_secs(10), handle)
        .await
        .expect("peer did not observe socket close")
        .unwrap()
        .unwrap();
    assert_eq!(closed, 0);
}

#[tokio::test]
async fn cancelled_http_request_closes_socket() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let p = proxy(listener.local_addr().unwrap());
    let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
    let peer = tokio::spawn(async move {
        let (mut s, _) = tokio::time::timeout(Duration::from_secs(10), listener.accept())
            .await
            .unwrap()
            .unwrap();
        let mut bytes = Vec::new();
        while !bytes.ends_with(b"\r\n\r\n") {
            assert!(bytes.len() < 2048);
            let mut b = [0];
            tokio::time::timeout(Duration::from_secs(10), s.read_exact(&mut b))
                .await
                .unwrap()
                .unwrap();
            bytes.push(b[0]);
        }
        ready_tx.send(()).unwrap();
        tokio::time::timeout(Duration::from_secs(10), s.read(&mut [0]))
            .await
            .unwrap()
            .unwrap()
    });
    let url = Url::parse("http://no-dns.invalid/wait").unwrap();
    let mut future = Box::pin(request(&p, &url, empty_request(&url), deadline()));
    tokio::select! { result = &mut future => panic!("request unexpectedly completed: {}", result.is_ok()), _ = ready_rx => {} }
    drop(future);
    assert_eq!(
        tokio::time::timeout(Duration::from_secs(10), peer)
            .await
            .unwrap()
            .unwrap(),
        0
    );
}
