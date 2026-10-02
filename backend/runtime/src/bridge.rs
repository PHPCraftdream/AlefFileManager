// SPDX-License-Identifier: GPL-3.0-or-later
use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use http::{header, HeaderValue};
use ipc_channel::ipc;
use net_traits::request::{BodyChunkRequest, BodyChunkResponse, RequestBody};
use serde::Deserialize;
use serde_json::{json, Value};
use servo::protocol_handler::{
    DoneChannel, FetchContext, HttpStatus, ProtocolHandler, ProtocolRegistry, Request,
    ResourceFetchTiming, Response, ResponseBody,
};
use subtle::ConstantTimeEq;
use tokio::sync::Semaphore;
use tokio::task::JoinHandle;
use url::Url;

use crate::Commands;

const MAX_REQUEST_BYTES: usize = 256 * 1024;

pub struct Bridge {
    pub entry_url: Url,
    registry: Option<ProtocolRegistry>,
    commands: Arc<Commands>,
}

impl Bridge {
    pub async fn new(
        commands: Commands,
        assets: Option<&Path>,
        development_url: Option<Url>,
    ) -> io::Result<Self> {
        let assets = match assets {
            Some(path) => {
                let path = tokio::fs::canonicalize(path).await?;
                if !tokio::fs::try_exists(path.join("index.html")).await? {
                    return Err(io::Error::new(
                        io::ErrorKind::NotFound,
                        "Build the frontend before starting",
                    ));
                }
                Some(path)
            }
            None => None,
        };
        let mut random = [0u8; 32];
        getrandom::fill(&mut random).map_err(io::Error::other)?;
        let mut token = String::with_capacity(64);
        const HEX: &[u8; 16] = b"0123456789abcdef";
        for byte in random {
            token.push(char::from(HEX[usize::from(byte >> 4)]));
            token.push(char::from(HEX[usize::from(byte & 15)]));
        }
        let mut entry_url =
            development_url.unwrap_or_else(|| Url::parse("native://app/").expect("constant URL"));
        let fragment = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("capability", &token)
            .finish();
        entry_url.set_fragment(Some(&fragment));
        let commands = Arc::new(commands);
        let handler = MemoryProtocol {
            assets,
            token,
            commands: commands.clone(),
            handle: tokio::runtime::Handle::current(),
            admission: Arc::new(Semaphore::new(32)),
        };
        let mut registry = ProtocolRegistry::with_internal_protocols();
        registry.register("native", handler).map_err(|error| {
            io::Error::other(format!("Protocol registration failed: {error:?}"))
        })?;
        Ok(Self {
            entry_url,
            registry: Some(registry),
            commands,
        })
    }

    pub(crate) fn take_registry(&mut self) -> io::Result<ProtocolRegistry> {
        self.registry
            .take()
            .ok_or_else(|| io::Error::other("Bridge already attached to a window"))
    }

    /// Call after the Servo window has closed. Fjall owners are dropped off the UI thread.
    pub async fn shutdown(self) -> io::Result<()> {
        tokio::task::spawn_blocking(move || drop((self.registry, self.commands)))
            .await
            .map_err(io::Error::other)
    }
}

struct MemoryProtocol {
    assets: Option<PathBuf>,
    token: String,
    commands: Arc<Commands>,
    handle: tokio::runtime::Handle,
    admission: Arc<Semaphore>,
}

// Dropping an interrupted fetch must not detach its command task.
struct OwnedTask<T>(JoinHandle<T>);
impl<T> Drop for OwnedTask<T> {
    fn drop(&mut self) {
        self.0.abort();
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Invocation {
    command: String,
    arguments: Value,
}

impl ProtocolHandler for MemoryProtocol {
    fn is_fetchable(&self) -> bool {
        true
    }
    fn is_secure(&self) -> bool {
        true
    }

    fn load<'a>(
        &'a self,
        request: &'a mut Request,
        _: &mut DoneChannel,
        _: &FetchContext,
    ) -> Pin<Box<dyn Future<Output = Response> + Send + 'a>> {
        Box::pin(async move {
            let mut response = Response::new(
                request.current_url().clone(),
                ResourceFetchTiming::new(request.timing_type()),
            );
            let url = request.current_url().clone();
            let result = if url.host_str() == Some("invoke") && url.path() == "/" {
                self.invoke(request).await.and_then(|value| {
                    serde_json::to_vec(&value)
                        .map(|bytes| (bytes, "application/json".to_owned()))
                        .map_err(io::Error::other)
                })
            } else if url.host_str() == Some("app") && request.method == http::Method::GET {
                let assets = self.assets.clone();
                let path = url.path().trim_start_matches('/').to_owned();
                let mut task = OwnedTask(
                    self.handle
                        .spawn(async move { read_asset(assets, path).await }),
                );
                (&mut task.0)
                    .await
                    .map_err(io::Error::other)
                    .and_then(|result| result)
            } else {
                Err(io::Error::new(
                    io::ErrorKind::NotFound,
                    "Unknown private resource",
                ))
            };
            let (status, bytes, content_type) = match result {
                Ok((bytes, content_type)) => (200, bytes, content_type),
                Err(error) => {
                    let status = match error.kind() {
                        io::ErrorKind::PermissionDenied => 403,
                        io::ErrorKind::NotFound => 404,
                        io::ErrorKind::InvalidInput | io::ErrorKind::InvalidData => 400,
                        io::ErrorKind::WouldBlock => 429,
                        _ => 500,
                    };
                    (
                        status,
                        serde_json::to_vec(&json!({"error": error.to_string()}))
                            .expect("JSON string"),
                        "application/json".to_owned(),
                    )
                }
            };
            response.status = HttpStatus::new_raw(status, vec![]);
            response.headers.insert(
                header::CONTENT_TYPE,
                HeaderValue::from_str(&content_type).expect("MIME header"),
            );
            response.headers.insert(
                header::ACCESS_CONTROL_ALLOW_ORIGIN,
                HeaderValue::from_static("*"),
            );
            response.headers.insert(
                header::ACCESS_CONTROL_ALLOW_METHODS,
                HeaderValue::from_static("POST, OPTIONS"),
            );
            response.headers.insert(
                header::ACCESS_CONTROL_ALLOW_HEADERS,
                HeaderValue::from_static("authorization, content-type"),
            );
            response.headers.insert(
                header::X_CONTENT_TYPE_OPTIONS,
                HeaderValue::from_static("nosniff"),
            );
            if url.host_str() == Some("app") {
                response.headers.insert(header::CONTENT_SECURITY_POLICY, HeaderValue::from_static(
                    "default-src native:; connect-src native:; img-src native: data:; style-src native: 'unsafe-inline'; object-src 'none'; frame-src 'none'; base-uri 'none'"));
            }
            *response.body.lock() = ResponseBody::Done(bytes);
            response
        })
    }
}

impl MemoryProtocol {
    async fn invoke(&self, request: &mut Request) -> io::Result<Value> {
        if request.method == http::Method::OPTIONS {
            return Ok(Value::Null);
        }
        if request.method != http::Method::POST {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Invoke requires POST",
            ));
        }
        authorize(&self.token, &request.headers)?;
        let permit = self.admission.clone().try_acquire_owned().map_err(|_| {
            io::Error::new(io::ErrorKind::WouldBlock, "Native command capacity reached")
        })?;
        let body = request.body.take().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "Missing invocation body")
        })?;
        let commands = self.commands.clone();
        let mut task = OwnedTask(self.handle.spawn(async move {
            let _permit = permit;
            let bytes = tokio::time::timeout(Duration::from_secs(10), read_body(body))
                .await
                .map_err(|_| {
                    io::Error::new(io::ErrorKind::TimedOut, "Invocation body timed out")
                })??;
            let invocation: Invocation = serde_json::from_slice(&bytes)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
            commands
                .invoke(&invocation.command, invocation.arguments)
                .await
        }));
        (&mut task.0).await.map_err(io::Error::other)?
    }
}

struct BodySession {
    body: RequestBody,
    sender: ipc::IpcSender<BodyChunkRequest>,
    complete: bool,
}
impl Drop for BodySession {
    fn drop(&mut self) {
        if !self.complete {
            let _ = self.sender.send(BodyChunkRequest::Error);
        }
        self.body.close_stream();
    }
}

async fn read_body(body: RequestBody) -> io::Result<Vec<u8>> {
    if body.len().is_some_and(|length| length > MAX_REQUEST_BYTES) {
        body.close_stream();
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Invocation body too large",
        ));
    }
    let sender = body
        .clone_stream()
        .lock()
        .clone()
        .ok_or_else(|| io::Error::other("Body stream closed"))?;
    let mut session = BodySession {
        body,
        sender,
        complete: false,
    };
    let (sender, receiver) = ipc::channel::<BodyChunkResponse>().map_err(io::Error::other)?;
    session
        .sender
        .send(BodyChunkRequest::Connect(sender))
        .map_err(io::Error::other)?;
    let mut stream = receiver.to_stream();
    let mut bytes = Vec::new();
    loop {
        session
            .sender
            .send(BodyChunkRequest::Chunk)
            .map_err(io::Error::other)?;
        match stream.next().await {
            Some(Ok(BodyChunkResponse::Chunk(chunk))) => {
                if chunk.len() > MAX_REQUEST_BYTES - bytes.len() {
                    return Err(io::Error::new(
                        io::ErrorKind::InvalidInput,
                        "Invocation body too large",
                    ));
                }
                bytes.extend_from_slice(&chunk);
            }
            Some(Ok(BodyChunkResponse::Done)) => {
                session.complete = true;
                return Ok(bytes);
            }
            _ => {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidData,
                    "Invocation body stream failed",
                ))
            }
        }
    }
}

async fn read_asset(assets: Option<PathBuf>, requested: String) -> io::Result<(Vec<u8>, String)> {
    let root = assets
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "Assets are served by Rsbuild"))?;
    let requested = if requested.is_empty() {
        "index.html"
    } else {
        &requested
    };
    let path = tokio::fs::canonicalize(root.join(requested)).await?;
    if !path.starts_with(&root) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Asset outside frontend root",
        ));
    }
    let mime = mime_guess::from_path(&path)
        .first_or_octet_stream()
        .to_string();
    Ok((tokio::fs::read(path).await?, mime))
}

fn authorize(token: &str, headers: &http::HeaderMap) -> io::Result<()> {
    let provided = headers
        .get(header::AUTHORIZATION)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .unwrap_or("");
    if !bool::from(provided.as_bytes().ct_eq(token.as_bytes())) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Private bridge capability required",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rejects_missing_foreign_and_malformed_capabilities() {
        let token = "process-private-capability";
        for value in [
            None,
            Some("Bearer foreign-capability"),
            Some("process-private-capability"),
        ] {
            let mut headers = http::HeaderMap::new();
            if let Some(value) = value {
                headers.insert(header::AUTHORIZATION, HeaderValue::from_static(value));
            }
            assert_eq!(
                authorize(token, &headers).expect_err("denied").kind(),
                io::ErrorKind::PermissionDenied
            );
        }
        let mut headers = http::HeaderMap::new();
        headers.insert(
            header::AUTHORIZATION,
            HeaderValue::from_static("Bearer process-private-capability"),
        );
        assert!(authorize(token, &headers).is_ok());
    }

    #[tokio::test]
    async fn refuses_asset_parent_traversal() {
        let directory = tempfile::tempdir().expect("assets");
        let root = directory.path().join("frontend");
        tokio::fs::create_dir(&root).await.expect("frontend");
        tokio::fs::write(directory.path().join("secret"), b"private")
            .await
            .expect("outside asset");
        let root = tokio::fs::canonicalize(root).await.expect("root");
        let error = read_asset(Some(root), "../secret".to_owned())
            .await
            .expect_err("outside root");
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
    }
}
