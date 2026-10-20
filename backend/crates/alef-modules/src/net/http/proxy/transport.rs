// SPDX-License-Identifier: MIT OR Apache-2.0
//! A bounded, one-connection transport through an explicit HTTP proxy.
use std::{
    pin::Pin,
    sync::Arc,
    task::{Context, Poll},
    time::Duration,
};

use bytes::Bytes;
use hyper::{
    body::{Body, Frame, Incoming, SizeHint},
    header::HOST,
    Request, Response, Uri,
};
use hyper_util::rt::TokioIo;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    net::TcpStream,
    task::JoinHandle,
};
use url::Url;

use crate::net::http::{
    body::RequestBody,
    proxy::{authority, Proxy},
    timeout_error,
};

pub(super) const HEAD_LIMIT: usize = 16_384;

struct Driver(JoinHandle<()>);
impl Drop for Driver {
    fn drop(&mut self) {
        self.0.abort();
    }
}

pub(in crate::net::http) struct ResponseBody {
    incoming: Incoming,
    driver: Option<Driver>,
}
impl ResponseBody {
    pub(in crate::net::http) fn direct(incoming: Incoming) -> Self {
        Self {
            incoming,
            driver: None,
        }
    }
}
impl Body for ResponseBody {
    type Data = Bytes;
    type Error = hyper::Error;
    fn poll_frame(
        mut self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Option<Result<Frame<Self::Data>, Self::Error>>> {
        Pin::new(&mut self.incoming).poll_frame(cx)
    }
    fn is_end_stream(&self) -> bool {
        self.incoming.is_end_stream()
    }
    fn size_hint(&self) -> SizeHint {
        self.incoming.size_hint()
    }
}
impl Drop for ResponseBody {
    fn drop(&mut self) {
        self.driver.take();
    }
}
trait Io: AsyncRead + AsyncWrite + Unpin + Send {}
impl<T: AsyncRead + AsyncWrite + Unpin + Send> Io for T {}
type BoxIo = Box<dyn Io>;
fn net() -> alef_core::AlefError {
    alef_core::AlefError::new(alef_core::ErrorCode::Network, "the proxied request failed")
}

pub(super) fn tls_config() -> Result<Arc<rustls::ClientConfig>, alef_core::AlefError> {
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let roots = rustls::RootCertStore {
        roots: webpki_roots::TLS_SERVER_ROOTS.to_vec(),
    };
    let mut config = rustls::ClientConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .map_err(|_| net())?
        .with_root_certificates(roots)
        .with_no_client_auth();
    config.alpn_protocols = vec![b"http/1.1".to_vec()];
    Ok(Arc::new(config))
}

pub(in crate::net::http) async fn request(
    proxy: &Proxy,
    url: &Url,
    request: Request<RequestBody>,
    deadline: tokio::time::Instant,
) -> Result<Response<ResponseBody>, alef_core::AlefError> {
    request_with_config(
        proxy,
        url,
        request,
        deadline,
        Duration::from_secs(10),
        tls_config()?,
    )
    .await
}

/// Cancellation-safe: callers may wrap this helper in a timeout.
pub(super) async fn request_with_config(
    proxy: &Proxy,
    url: &Url,
    mut request: Request<RequestBody>,
    deadline: tokio::time::Instant,
    handshake_limit: Duration,
    config: Arc<rustls::ClientConfig>,
) -> Result<Response<ResponseBody>, alef_core::AlefError> {
    let https = url.scheme() == "https";
    let host = url.host_str().ok_or_else(net)?;
    let tls_host = host.trim_start_matches('[').trim_end_matches(']');
    let authority = authority(host, url.port_or_known_default().ok_or_else(net)?);
    let io = tokio::time::timeout_at(
        (tokio::time::Instant::now() + handshake_limit).min(deadline),
        async {
            let mut stream =
                TcpStream::connect((proxy.address.trim_matches(['[', ']']), proxy.port))
                    .await
                    .map_err(|_| net())?;
            let io: BoxIo = if https {
                stream
                    .write_all(
                        format!("CONNECT {authority} HTTP/1.1\r\nHost: {authority}\r\n\r\n")
                            .as_bytes(),
                    )
                    .await
                    .map_err(|_| net())?;
                let mut head = Vec::with_capacity(256);
                while head.len() < HEAD_LIMIT {
                    let mut byte = [0u8; 1];
                    stream.read_exact(&mut byte).await.map_err(|_| net())?;
                    head.push(byte[0]);
                    if head.ends_with(b"\r\n\r\n") {
                        break;
                    }
                }
                if !head.ends_with(b"\r\n\r\n") {
                    return Err(net());
                }
                let line = std::str::from_utf8(&head)
                    .map_err(|_| net())?
                    .split("\r\n")
                    .next()
                    .ok_or_else(net)?;
                let mut fields = line.split_ascii_whitespace();
                let version = fields.next().ok_or_else(net)?;
                let status: u16 = fields.next().ok_or_else(net)?.parse().map_err(|_| net())?;
                if !(200..300).contains(&status) {
                    return Err(alef_core::AlefError::new(
                        alef_core::ErrorCode::Network,
                        "the proxy refused the CONNECT tunnel",
                    ));
                }
                if !matches!(version, "HTTP/1.0" | "HTTP/1.1") {
                    return Err(net());
                }
                let name = rustls::pki_types::ServerName::try_from(tls_host.to_owned())
                    .map_err(|_| net())?;
                Box::new(
                    tokio_rustls::TlsConnector::from(config.clone())
                        .connect(name, stream)
                        .await
                        .map_err(|_| net())?,
                )
            } else {
                Box::new(stream)
            };
            Ok(io)
        },
    )
    .await
    .map_err(|_| timeout_error())??;
    tokio::time::timeout_at(deadline, async {
        let mut target = url.clone();
        target.set_fragment(None);
        *request.uri_mut() = if https {
            Uri::try_from(target[url::Position::BeforePath..].to_owned()).map_err(|_| net())?
        } else {
            Uri::try_from(target.as_str()).map_err(|_| net())?
        };
        request.headers_mut().insert(
            HOST,
            hyper::header::HeaderValue::from_str(&authority).map_err(|_| net())?,
        );
        let (mut sender, connection) = hyper::client::conn::http1::Builder::new()
            .max_buf_size(32_768)
            .handshake(TokioIo::new(io))
            .await
            .map_err(|_| net())?;
        let driver = Driver(tokio::spawn(async move {
            let _ = connection.await;
        }));
        let response = sender.send_request(request).await.map_err(|_| net())?;
        let (parts, incoming) = response.into_parts();
        Ok(Response::from_parts(
            parts,
            ResponseBody {
                incoming,
                driver: Some(driver),
            },
        ))
    })
    .await
    .map_err(|_| timeout_error())?
}
