// SPDX-License-Identifier: MIT OR Apache-2.0
//! Explicit proxy requests through the registry.
use super::*;
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
};

struct Peer(Option<tokio::task::JoinHandle<TcpListener>>);
impl Drop for Peer {
    fn drop(&mut self) {
        if let Some(handle) = self.0.take() {
            handle.abort();
        }
    }
}
impl Peer {
    async fn finish(mut self) -> TcpListener {
        tokio::time::timeout(Duration::from_secs(10), self.0.take().unwrap())
            .await
            .expect("proxy peer timed out")
            .expect("proxy peer failed")
    }
}

async fn listener() -> (TcpListener, String) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let url = format!("http://{}", listener.local_addr().unwrap());
    (listener, url)
}

async fn read_headers(stream: &mut TcpStream) -> String {
    tokio::time::timeout(Duration::from_secs(10), async {
        let mut bytes = Vec::new();
        while bytes.len() < 2048 && !bytes.ends_with(b"\r\n\r\n") {
            let mut byte = [0];
            stream.read_exact(&mut byte).await.unwrap();
            bytes.push(byte[0]);
        }
        assert!(bytes.ends_with(b"\r\n\r\n"), "header exceeded limit");
        String::from_utf8(bytes).unwrap()
    })
    .await
    .expect("header read timed out")
}

async fn assert_no_connection(listener: &TcpListener) {
    assert!(
        tokio::time::timeout(Duration::from_millis(30), listener.accept())
            .await
            .is_err()
    );
}

#[tokio::test]
async fn closing_a_stalled_direct_response_stream_closes_the_socket() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let (listener, url) = listener().await;
        let peer = Peer(Some(tokio::spawn(async move {
            tokio::time::timeout(Duration::from_secs(10), async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                read_headers(&mut stream).await;
                stream
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n")
                    .await
                    .unwrap();
                let mut byte = [0];
                match stream.read(&mut byte).await {
                    Ok(0) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => {}
                    Ok(count) => panic!("unexpected body bytes: {count}"),
                    Err(error) => panic!("socket did not close cleanly: {error}"),
                }
                listener
            })
            .await
            .expect("stalled direct peer timed out")
        })));
        let app = app(&[format!("{url}/*")]).await;
        let head = ask(
            &app,
            json!({ "url": format!("{url}/stalled"), "timeoutMs": 2000 }),
        )
        .await
        .unwrap();
        let stream = alef_core::ids::StreamId(head["stream"].as_u64().unwrap());
        app.session().streams().close(stream).unwrap();
        drop(peer.finish().await);
    })
    .await
    .expect("stalled direct response test timed out");
}

#[tokio::test]
async fn closing_a_stalled_download_stream_closes_the_socket_and_removes_the_file() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let (listener, url) = listener().await;
        let peer = Peer(Some(tokio::spawn(async move {
            tokio::time::timeout(Duration::from_secs(10), async move {
                let (mut stream, _) = listener.accept().await.unwrap();
                read_headers(&mut stream).await;
                stream
                    .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n")
                    .await
                    .unwrap();
                let mut byte = [0];
                match stream.read(&mut byte).await {
                    Ok(0) => {}
                    Err(error) if error.kind() == std::io::ErrorKind::ConnectionReset => {}
                    Ok(count) => panic!("unexpected body bytes: {count}"),
                    Err(error) => panic!("socket did not close cleanly: {error}"),
                }
                listener
            })
            .await
            .expect("stalled download peer timed out")
        })));
        let directory = tempfile::tempdir().unwrap();
        let target = directory.path().join("stalled.bin");
        let app = Fixture::new(
            Some(&manifest(
                &[format!("{url}/*")],
                &[format!("{}/**", directory.path().display().to_string().replace('\\', "/"))],
            )),
            &[],
        )
        .await;
        let started = app
            .call(
                "http.download",
                json!({ "url": format!("{url}/stalled"), "path": target.to_string_lossy(), "timeoutMs": 2000 }),
            )
            .await
            .unwrap();
        assert!(target.exists(), "download created its target before streaming");
        let stream = alef_core::ids::StreamId(started["stream"].as_u64().unwrap());
        app.session().streams().close(stream).unwrap();
        drop(peer.finish().await);
        tokio::time::timeout(Duration::from_secs(10), async {
            while target.exists() {
                tokio::task::yield_now().await;
            }
        })
        .await
        .expect("cancelled download file was not removed");
        assert!(!target.exists());
    })
    .await
    .expect("stalled download test timed out");
}

#[tokio::test]
async fn proxy_scope_is_checked_before_connecting() {
    let (listener, proxy) = listener().await;
    let app = app(&["http://target.invalid/*".to_owned()]).await;
    let result = ask(
        &app,
        json!({"url":"http://target.invalid/a", "proxy":proxy, "timeoutMs":1000}),
    )
    .await;
    assert_eq!(code(result), ErrorCode::PermissionDenied);
    assert_no_connection(&listener).await;
}

#[tokio::test]
async fn allowed_proxy_receives_absolute_form_without_target_dns() {
    let (listener, proxy) = listener().await;
    let peer = Peer(Some(tokio::spawn(async move {
        tokio::time::timeout(Duration::from_secs(10), async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let head = read_headers(&mut stream).await;
            assert!(
                head.starts_with("GET http://target.invalid/a HTTP/1.1\r\n"),
                "{head:?}"
            );
            assert!(
                head.to_ascii_lowercase()
                    .contains("\r\nhost: target.invalid:80\r\n"),
                "{head:?}"
            );
            stream
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok")
                .await
                .unwrap();
            listener
        })
        .await
        .expect("proxy peer timed out")
    })));
    let app = app(&["http://target.invalid/*".to_owned(), format!("{proxy}/*")]).await;
    let head = ask(
        &app,
        json!({"url":"http://target.invalid/a", "proxy":proxy, "timeoutMs":1000}),
    )
    .await
    .unwrap();
    assert_eq!(body_of(&app, &head).await, b"ok");
    drop(peer.finish().await);
}

#[tokio::test]
async fn redirect_target_is_rechecked_while_proxy_is_retained() {
    let (listener, proxy) = listener().await;
    let peer = Peer(Some(tokio::spawn(async move {
        tokio::time::timeout(Duration::from_secs(10), async move {
            let (mut stream, _) = listener.accept().await.unwrap();
            let head = read_headers(&mut stream).await;
            assert!(head.starts_with("GET http://target.invalid/first HTTP/1.1\r\n"), "{head:?}");
            stream.write_all(b"HTTP/1.1 302 Found\r\nLocation: http://denied.invalid/a\r\nContent-Length: 0\r\nConnection: close\r\n\r\n").await.unwrap();
            drop(stream);
            listener
        }).await.expect("proxy peer timed out")
    })));
    let app = app(&["http://target.invalid/*".to_owned(), format!("{proxy}/*")]).await;
    let result = ask(
        &app,
        json!({"url":"http://target.invalid/first", "proxy":proxy, "timeoutMs":1000}),
    )
    .await;
    assert_eq!(code(result), ErrorCode::PermissionDenied);
    assert_no_connection(&peer.finish().await).await;
}
