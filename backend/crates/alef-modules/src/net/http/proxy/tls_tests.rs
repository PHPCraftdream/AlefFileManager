// SPDX-License-Identifier: MIT OR Apache-2.0
use std::{sync::Arc, time::Duration};

use bytes::Bytes;
use http_body_util::{BodyExt, Empty};
use hyper::Request;
use rustls::pki_types::{pem::PemObject, CertificateDer, PrivateKeyDer};
use tokio::{
    io::{AsyncReadExt, AsyncWriteExt},
    net::{TcpListener, TcpStream},
    task::JoinHandle,
};
use tokio_rustls::TlsAcceptor;

use super::{transport::request_with_config, Proxy};

struct AbortOnDrop(JoinHandle<bool>);
impl Drop for AbortOnDrop {
    fn drop(&mut self) {
        self.0.abort();
    }
}

fn fixture(name: &str) -> &'static [u8] {
    match name {
        "ca.pem" => include_bytes!("../../../../tests/fixtures/tls/ca.pem"),
        "server.pem" => include_bytes!("../../../../tests/fixtures/tls/server.pem"),
        _ => include_bytes!("../../../../tests/fixtures/tls/server.key"),
    }
}

fn client_config() -> Arc<rustls::ClientConfig> {
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let mut roots = rustls::RootCertStore::empty();
    for cert in CertificateDer::pem_slice_iter(fixture("ca.pem")) {
        roots.add(cert.unwrap()).unwrap();
    }
    let mut config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_root_certificates(roots)
        .with_no_client_auth();
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Arc::new(config)
}

fn server_config() -> Arc<rustls::ServerConfig> {
    let certs: Vec<_> = CertificateDer::pem_slice_iter(fixture("server.pem"))
        .map(Result::unwrap)
        .collect();
    let key = PrivateKeyDer::from_pem_slice(fixture("server.key")).unwrap();
    Arc::new(
        rustls::ServerConfig::builder_with_provider(Arc::new(
            rustls::crypto::aws_lc_rs::default_provider(),
        ))
        .with_safe_default_protocol_versions()
        .unwrap()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .unwrap(),
    )
}

async fn proxy(peer: TcpStream, authority: &'static str, success_case: bool) -> bool {
    let mut peer = peer;
    let mut head = Vec::new();
    while !head.ends_with(b"\r\n\r\n") && head.len() < 16_384 {
        let mut byte = [0];
        if peer.read_exact(&mut byte).await.is_err() {
            return false;
        }
        head.push(byte[0]);
    }
    let expected = format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n\r\n");
    if head != expected.as_bytes()
        || peer
            .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
            .await
            .is_err()
    {
        return false;
    }
    match TlsAcceptor::from(server_config()).accept(peer).await {
        Err(_) => !success_case,
        Ok(mut tls) if success_case => {
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") && request.len() < 16_384 {
                let mut byte = [0];
                if tls.read_exact(&mut byte).await.is_err() {
                    return false;
                }
                request.push(byte[0]);
            }
            let request = String::from_utf8_lossy(&request).to_ascii_lowercase();
            if !request.starts_with("get /inside http/1.1\r\n")
                || !request.contains("\r\nhost: localhost:443\r\n")
            {
                return false;
            }
            tls.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\ninside",
            )
            .await
            .is_ok()
        }
        Ok(_) => false,
    }
}

fn proxy_spec(address: std::net::SocketAddr) -> Proxy {
    Proxy {
        url: url::Url::parse(&format!("http://{address}")).unwrap(),
        address: address.ip().to_string(),
        port: address.port(),
    }
}

async fn run(
    url: &'static str,
    config: Arc<rustls::ClientConfig>,
    authority: &'static str,
    success_case: bool,
) -> (Result<Bytes, alef_core::AlefError>, AbortOnDrop) {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let address = listener.local_addr().unwrap();
    let peer = AbortOnDrop(tokio::spawn(async move {
        let (stream, _) = listener.accept().await.unwrap();
        proxy(stream, authority, success_case).await
    }));
    let request = Request::builder()
        .body(
            Empty::<Bytes>::new()
                .map_err(|never| match never {})
                .boxed(),
        )
        .unwrap();
    let result = async {
        let response = request_with_config(
            &proxy_spec(address),
            &url::Url::parse(url).unwrap(),
            request,
            tokio::time::Instant::now() + Duration::from_secs(10),
            Duration::from_secs(10),
            config,
        )
        .await?;
        let body = response
            .into_body()
            .collect()
            .await
            .map_err(|_| alef_core::AlefError::new(alef_core::ErrorCode::Network, "body failed"))?
            .to_bytes();
        Ok(body)
    }
    .await;
    (result, peer)
}

#[tokio::test]
async fn closing_a_stalled_connect_response_stream_closes_the_tunnel() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let peer = AbortOnDrop(tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut peer = stream;
            let mut head = Vec::new();
            while !head.ends_with(b"\r\n\r\n") && head.len() < 16_384 {
                let mut byte = [0];
                if peer.read_exact(&mut byte).await.is_err() {
                    return false;
                }
                head.push(byte[0]);
            }
            if head != b"CONNECT localhost:443 HTTP/1.1\r\nHost: localhost:443\r\n\r\n"
                || peer
                    .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                    .await
                    .is_err()
            {
                return false;
            }
            let Ok(mut tls) = TlsAcceptor::from(server_config()).accept(peer).await else {
                return false;
            };
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") && request.len() < 16_384 {
                let mut byte = [0];
                if tls.read_exact(&mut byte).await.is_err() {
                    return false;
                }
                request.push(byte[0]);
            }
            let request = String::from_utf8_lossy(&request).to_ascii_lowercase();
            if !request.starts_with("get /inside http/1.1\r\n")
                || !request.contains("\r\nhost: localhost:443\r\n")
            {
                return false;
            }
            if tls
                .write_all(b"HTTP/1.1 200 OK\r\nContent-Length: 100\r\n\r\n")
                .await
                .is_err()
            {
                return false;
            }
            let mut byte = [0];
            match tls.read(&mut byte).await {
                Ok(0) => true,
                Err(error) => matches!(
                    error.kind(),
                    std::io::ErrorKind::UnexpectedEof | std::io::ErrorKind::ConnectionReset
                ),
                Ok(_) => false,
            }
        }));
        let request = Request::builder()
            .body(
                Empty::<Bytes>::new()
                    .map_err(|never| match never {})
                    .boxed(),
            )
            .unwrap();
        let response = request_with_config(
            &proxy_spec(address),
            &url::Url::parse("https://localhost:443/inside").unwrap(),
            request,
            tokio::time::Instant::now() + Duration::from_secs(10),
            Duration::from_secs(10),
            client_config(),
        )
        .await
        .unwrap();
        let hub = alef_core::session::streams::StreamHub::new(64 * 1024, 16 * 1024);
        let (writer, id) = hub.open_outgoing();
        super::super::body::pump(response.into_body(), writer);
        hub.close(id).unwrap();
        let mut peer = peer;
        assert!((&mut peer.0).await.unwrap());
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn a_completed_proxy_handshake_does_not_limit_waiting_for_response_headers() {
    tokio::time::timeout(Duration::from_secs(30), async {
        let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let (ready_tx, ready_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = tokio::sync::oneshot::channel();
        let peer = AbortOnDrop(tokio::spawn(async move {
            let (stream, _) = listener.accept().await.unwrap();
            let mut peer = stream;
            let mut head = Vec::new();
            while !head.ends_with(b"\r\n\r\n") && head.len() < 16_384 {
                let mut byte = [0];
                if peer.read_exact(&mut byte).await.is_err() {
                    return false;
                }
                head.push(byte[0]);
            }
            if head != b"CONNECT localhost:443 HTTP/1.1\r\nHost: localhost:443\r\n\r\n"
                || peer
                    .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
                    .await
                    .is_err()
            {
                return false;
            }
            let Ok(mut tls) = TlsAcceptor::from(server_config()).accept(peer).await else {
                return false;
            };
            let mut request = Vec::new();
            while !request.ends_with(b"\r\n\r\n") && request.len() < 16_384 {
                let mut byte = [0];
                if tls.read_exact(&mut byte).await.is_err() {
                    return false;
                }
                request.push(byte[0]);
            }
            let request = String::from_utf8_lossy(&request).to_ascii_lowercase();
            if !request.starts_with("get /inside http/1.1\r\n")
                || !request.contains("\r\nhost: localhost:443\r\n")
            {
                return false;
            }
            if ready_tx.send(()).is_err() || release_rx.await.is_err() {
                return false;
            }
            tls.write_all(
                b"HTTP/1.1 200 OK\r\nContent-Length: 6\r\nConnection: close\r\n\r\ninside",
            )
            .await
            .is_ok()
        }));
        let request = Request::builder()
            .body(
                Empty::<Bytes>::new()
                    .map_err(|never| match never {})
                    .boxed(),
            )
            .unwrap();
        let proxy = proxy_spec(address);
        let url = url::Url::parse("https://localhost:443/inside").unwrap();
        let mut future = Box::pin(request_with_config(
            &proxy,
            &url,
            request,
            tokio::time::Instant::now() + Duration::from_secs(30),
            Duration::from_secs(1),
            client_config(),
        ));
        tokio::select! {
            ready = ready_rx => ready.unwrap(),
            result = &mut future => panic!("request completed before release: {}", result.is_ok()),
        }
        tokio::time::pause();
        tokio::time::advance(Duration::from_secs(2)).await;
        std::future::poll_fn(|cx| {
            use std::future::Future;
            assert!(
                future.as_mut().poll(cx).is_pending(),
                "handshake deadline leaked past completion"
            );
            std::task::Poll::Ready(())
        })
        .await;
        tokio::time::resume();
        release_tx.send(()).unwrap();
        let response = future.await.unwrap();
        assert_eq!(
            response.into_body().collect().await.unwrap().to_bytes(),
            Bytes::from_static(b"inside")
        );
        let mut peer = peer;
        assert!((&mut peer.0).await.unwrap());
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn trusted_fixture_ca_tls_over_connect() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let (result, mut guard) = run(
            "https://localhost:443/inside",
            client_config(),
            "localhost:443",
            true,
        )
        .await;
        assert_eq!(result.unwrap(), Bytes::from_static(b"inside"));
        assert!((&mut guard.0).await.unwrap());
    })
    .await
    .unwrap();
}

#[tokio::test]
async fn trusted_ca_does_not_skip_hostname_validation() {
    tokio::time::timeout(Duration::from_secs(10), async {
        let (result, mut guard) = run(
            "https://wrong.invalid:443/",
            client_config(),
            "wrong.invalid:443",
            false,
        )
        .await;
        assert!(matches!(result, Err(ref error) if error.code == alef_core::ErrorCode::Network));
        assert!((&mut guard.0).await.unwrap());
    })
    .await
    .unwrap();
}
