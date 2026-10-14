// SPDX-License-Identifier: MIT OR Apache-2.0
//! Bounded, redirect-free HTTP fetches for updater feeds.
use alef_core::{AlefError, ErrorCode};
use bytes::Bytes;
use http_body_util::BodyExt;
use hyper::{body::Incoming, header::CONTENT_LENGTH, Method, Request, Uri};
use hyper_rustls::{HttpsConnector, HttpsConnectorBuilder};
use hyper_util::{
    client::legacy::{connect::HttpConnector, Client},
    rt::TokioExecutor,
};
use std::{
    sync::{Arc, OnceLock},
    time::Duration,
};
use url::Url;

pub(super) const LIMIT: usize = 64 * 1024;
const DEADLINE: Duration = Duration::from_secs(30);
type Pool = Client<HttpsConnector<HttpConnector>, http_body_util::Empty<Bytes>>;
fn pool() -> &'static Pool {
    static POOL: OnceLock<Pool> = OnceLock::new();
    POOL.get_or_init(|| {
        let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
        let connector = HttpsConnectorBuilder::new()
            .with_provider_and_webpki_roots(provider)
            .expect("TLS provider")
            .https_or_http()
            .enable_http1()
            .enable_http2()
            .build();
        Client::builder(TokioExecutor::new()).build(connector)
    })
}
fn err(code: ErrorCode, message: &'static str) -> AlefError {
    AlefError::new(code, message)
}

pub(super) async fn get_response(url: &Url) -> Result<hyper::Response<Incoming>, AlefError> {
    let uri = Uri::try_from(url.as_str())
        .map_err(|_| err(ErrorCode::UpdateNetwork, "invalid updater endpoint"))?;
    let request = Request::builder()
        .method(Method::GET)
        .uri(uri)
        .body(http_body_util::Empty::new())
        .map_err(|_| err(ErrorCode::UpdateNetwork, "invalid updater request"))?;
    let response = pool()
        .request(request)
        .await
        .map_err(|_| err(ErrorCode::UpdateNetwork, "updater request failed"))?;
    if !response.status().is_success() {
        return Err(err(
            ErrorCode::UpdateNetwork,
            "updater server returned an error",
        ));
    }
    Ok(response)
}

pub(super) async fn get(url: &Url) -> Result<Vec<u8>, AlefError> {
    let deadline = tokio::time::Instant::now() + DEADLINE;
    let operation = async {
        let uri = Uri::try_from(url.as_str())
            .map_err(|_| err(ErrorCode::UpdateNetwork, "invalid updater endpoint"))?;
        let request = Request::builder()
            .method(Method::GET)
            .uri(uri)
            .body(http_body_util::Empty::new())
            .map_err(|_| err(ErrorCode::UpdateNetwork, "invalid updater request"))?;
        let response: hyper::Response<Incoming> = pool()
            .request(request)
            .await
            .map_err(|_| err(ErrorCode::UpdateNetwork, "updater request failed"))?;
        if !response.status().is_success() {
            return Err(err(
                ErrorCode::UpdateNetwork,
                "updater server returned an error",
            ));
        }
        if response
            .headers()
            .get(CONTENT_LENGTH)
            .and_then(|v| v.to_str().ok())
            .and_then(|s| s.parse::<u64>().ok())
            .is_some_and(|n| n > LIMIT as u64)
        {
            return Err(err(
                ErrorCode::UpdateTooLarge,
                "updater document exceeds 64 KiB",
            ));
        }
        let mut body = response.into_body();
        let mut bytes = Vec::new();
        while let Some(frame) = body.frame().await {
            let frame =
                frame.map_err(|_| err(ErrorCode::UpdateNetwork, "updater response body failed"))?;
            if let Some(data) = frame.data_ref() {
                if bytes.len().saturating_add(data.len()) > LIMIT {
                    return Err(err(
                        ErrorCode::UpdateTooLarge,
                        "updater document exceeds 64 KiB",
                    ));
                }
                bytes.extend_from_slice(data);
            }
        }
        Ok(bytes)
    };
    tokio::select! {
        biased;
        _ = tokio::time::sleep_until(deadline) => Err(err(ErrorCode::UpdateTimeout, "updater request timed out")),
        result = operation => result,
    }
}
