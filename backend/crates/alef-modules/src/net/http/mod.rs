// SPDX-License-Identifier: MIT OR Apache-2.0
//! `http`: requests from the program to servers on the network, which no CORS of a page restricts and
//! only `permissions.net.http` does: a URL pattern the manifest lists (and the user allowed), checked
//! for the first address and again for every redirect. The body of an answer comes as a stream with
//! credit, so that a big one never lies in memory; a body that is big goes up as a stream too.
//! `download` fills a file the same way, as `permissions.fs.write` allows. The right the user
//! substituted is a dead network: the request hangs until its timeout and then fails.
use std::{path::PathBuf, sync::Mutex, time::Duration};

use alef_core::{
    ids::ResourceId,
    registry::{command::Reply, context::CallContext, dispatch::Registry},
    security::{consent::Decision, permissions::Permission},
    session::resources::Resource,
    AlefError, ErrorCode,
};
use hyper::Method;
use serde::Deserialize;
use serde_json::json;
use tokio::{io::AsyncWriteExt, task::JoinHandle};

use crate::{json, ModuleContext};
use body::{bodiless, Payload};
use client::{execute, Answer, Authorize, Spec};
use spec::spec;

mod body;
mod client;
mod spec;

/// How long a request the user substituted hangs when it names no timeout.
const HANG: Duration = Duration::from_secs(30);
/// A progress frame of a download is sent after this many new bytes at the least.
const PROGRESS_STEP: u64 = 256 * 1024;

#[derive(Debug, Default, Deserialize, Clone, Copy, PartialEq, Eq)]
#[serde(rename_all = "lowercase")]
enum Redirect {
    #[default]
    Follow,
    Manual,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct RequestArgs {
    url: String,
    #[serde(default)]
    method: Option<String>,
    #[serde(default)]
    headers: Vec<(String, String)>,
    #[serde(default)]
    timeout_ms: Option<u64>,
    #[serde(default)]
    redirect: Redirect,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct DownloadArgs {
    url: String,
    path: String,
    #[serde(default)]
    headers: Vec<(String, String)>,
    #[serde(default)]
    timeout_ms: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Exchange {
    request: u64,
}

fn invalid(message: &str) -> AlefError {
    AlefError::new(ErrorCode::InvalidArgument, message)
}

fn timeout_error() -> AlefError {
    AlefError::new(ErrorCode::Timeout, "the server did not answer in time")
}

/// Holds an address against `permissions.net.http`; what the user substituted is no hop to follow.
fn authorizer(ctx: &CallContext) -> Authorize {
    let permissions = ctx.permissions.clone();
    let grants = ctx.grants();
    Box::new(
        move |url| match permissions.check(Permission::NetHttp, Some(url), &grants)? {
            Decision::Allow => Ok(()),
            _ => Err(AlefError::new(
                ErrorCode::PermissionDenied,
                "the address is outside what the application may reach",
            )),
        },
    )
}

/// The request, until its answer comes or its time is up.
async fn run(
    spec: Spec,
    payload: Payload,
    authorize: Authorize,
    timeout: Option<Duration>,
) -> Result<Answer, AlefError> {
    let exchange = execute(spec, payload, &authorize);
    match timeout {
        Some(limit) => tokio::time::timeout(limit, exchange)
            .await
            .map_err(|_| timeout_error())?,
        None => exchange.await,
    }
}

/// What the user substituted for the network: nothing answers.
async fn dead(timeout_ms: Option<u64>) -> Result<Answer, AlefError> {
    tokio::time::sleep(timeout_ms.map_or(HANG, Duration::from_millis)).await;
    Err(timeout_error())
}

fn head(answer: &Answer, stream: Option<u64>) -> serde_json::Value {
    let headers: Vec<[String; 2]> = answer
        .response
        .headers()
        .iter()
        .map(|(name, value)| {
            [
                name.as_str().to_owned(),
                String::from_utf8_lossy(value.as_bytes()).into_owned(),
            ]
        })
        .collect();
    let status = answer.response.status();
    json!({
        "status": status.as_u16(),
        "statusText": status.canonical_reason().unwrap_or(""),
        "url": answer.url.as_str(),
        "redirected": answer.redirected,
        "headers": headers,
        "stream": stream,
    })
}

/// Opens the stream of the body of an answer, unless it has none.
fn open_body(ctx: &CallContext, method: &Method, answer: Answer) -> serde_json::Value {
    if bodiless(method, answer.response.status()) {
        return head(&answer, None);
    }
    let (writer, id) = ctx.streams().open_outgoing();
    let reply = head(&answer, Some(id.0));
    body::pump(answer.response.into_body(), writer);
    reply
}

/// A request that waits for the body the page writes through a stream; its answer is asked for next.
struct Pending {
    task: Mutex<Option<JoinHandle<Result<Answer, AlefError>>>>,
    method: Method,
}

impl Resource for Pending {
    fn close(self: Box<Self>) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
        Box::pin(async move {
            if let Some(task) = self.task.lock().unwrap_or_else(|e| e.into_inner()).take() {
                task.abort();
            }
        })
    }
}

fn timeout_of(ms: Option<u64>) -> Option<Duration> {
    ms.map(Duration::from_millis)
}

/// Whether enough new bytes came since the last report of progress.
fn progress_due(received: u64, told: u64) -> bool {
    received - told >= PROGRESS_STEP
}

/// A file the answer of `download` is written to, a piece at a time, until the answer ends.
async fn fill(
    answer: Answer,
    file: std::fs::File,
    path: PathBuf,
    writer: alef_core::session::streams::StreamWriter,
) {
    use http_body_util::BodyExt;
    let total = answer
        .response
        .headers()
        .get(hyper::header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|text| text.parse::<u64>().ok());
    let mut body = answer.response.into_body();
    let mut file = tokio::fs::File::from_std(file);
    let (mut received, mut told) = (0_u64, 0_u64);
    let failure = loop {
        match body.frame().await {
            None => break None,
            Some(Err(_)) => {
                break Some(AlefError::new(
                    ErrorCode::Network,
                    "the connection broke while the body came",
                ))
            }
            Some(Ok(frame)) => {
                let Ok(data) = frame.into_data() else {
                    continue;
                };
                if let Err(error) = file.write_all(&data).await {
                    break Some(AlefError::from(error));
                }
                received += data.len() as u64;
                if progress_due(received, told) {
                    told = received;
                    if writer
                        .send_json(json!({ "received": received, "total": total }))
                        .await
                        .is_err()
                    {
                        // The page stopped listening (it aborted, or the document went away).
                        drop(file);
                        let _ = tokio::fs::remove_file(&path).await;
                        return;
                    }
                }
            }
        }
    };
    let failure = match failure {
        None => file.flush().await.err().map(AlefError::from),
        other => other,
    };
    drop(file);
    match failure {
        None => {
            if writer
                .send_json(json!({ "received": received, "total": total, "done": true }))
                .await
                .is_ok()
            {
                writer.end();
            }
        }
        Some(error) => {
            let _ = tokio::fs::remove_file(&path).await;
            writer.error(error);
        }
    }
}

pub(crate) fn register(registry: &mut Registry, context: &ModuleContext) -> Result<(), AlefError> {
    registry
        .command::<RequestArgs>("http.request")?
        .permission(Permission::NetHttp, |args| Some(args.url.clone()))
        .substitutes()
        .handler(|ctx, args| async move {
            let follow = args.redirect == Redirect::Follow;
            let spec = spec(&args.url, args.method.as_deref(), &args.headers, follow)?;
            let method = spec.method.clone();
            let answer = if ctx.decision() == Decision::Substitute {
                dead(args.timeout_ms).await?
            } else {
                let payload = ctx
                    .body()
                    .map_or(Payload::Empty, |bytes| Payload::Bytes(bytes.clone()));
                run(spec, payload, authorizer(&ctx), timeout_of(args.timeout_ms)).await?
            };
            Ok(Reply::Json(open_body(&ctx, &method, answer)))
        })?;

    registry
        .command::<RequestArgs>("http.start")?
        .permission(Permission::NetHttp, |args| Some(args.url.clone()))
        .substitutes()
        .handler(|ctx, args| async move {
            let follow = args.redirect == Redirect::Follow;
            let spec = spec(&args.url, args.method.as_deref(), &args.headers, follow)?;
            let method = spec.method.clone();
            let (reader, upload) = ctx.streams().open_incoming_reader();
            let timeout = timeout_of(args.timeout_ms);
            let task = if ctx.decision() == Decision::Substitute {
                // The body the page writes goes nowhere; the answer never comes.
                let mut reader = reader;
                tokio::spawn(async move { while reader.recv().await.is_some() {} });
                let hang = args.timeout_ms;
                tokio::spawn(dead(hang))
            } else {
                let payload = Payload::Stream(Some(body::from_stream(reader)));
                let authorize = authorizer(&ctx);
                tokio::spawn(run(spec, payload, authorize, timeout))
            };
            let id = ctx.resources().insert(Box::new(Pending {
                task: Mutex::new(Some(task)),
                method,
            }))?;
            json(&json!({ "request": id.0, "upload": upload.0 }))
        })?;

    registry
        .command::<Exchange>("http.response")?
        .handler(|ctx, args| async move {
            let id = ResourceId(args.request);
            let (task, method) = ctx.resources().with_as::<Pending, _>(id, |pending| {
                (
                    pending
                        .task
                        .lock()
                        .unwrap_or_else(|e| e.into_inner())
                        .take(),
                    pending.method.clone(),
                )
            })?;
            let task =
                task.ok_or_else(|| invalid("the answer of this request was asked for already"))?;
            let outcome = task
                .await
                .map_err(|error| AlefError::new(ErrorCode::Internal, error.to_string()))?;
            ctx.resources().take(id)?.close().await;
            Ok(Reply::Json(open_body(&ctx, &method, outcome?)))
        })?;

    let shadow = context.shadow.clone();
    registry
        .command::<DownloadArgs>("http.download")?
        .permission(Permission::NetHttp, |args| Some(args.url.clone()))
        .substitutes()
        .handler(move |ctx, args| {
            let shadow = shadow.clone();
            async move {
                let spec = spec(&args.url, None, &args.headers, true)?;
                if ctx.decision() == Decision::Substitute {
                    return Err(dead(args.timeout_ms)
                        .await
                        .err()
                        .unwrap_or_else(timeout_error));
                }
                let target = crate::data::fs::write_target(&shadow, &ctx, &args.path)?;
                let answer = run(
                    spec,
                    Payload::Empty,
                    authorizer(&ctx),
                    timeout_of(args.timeout_ms),
                )
                .await?;
                if !answer.response.status().is_success() {
                    return Err(AlefError::new(
                        ErrorCode::Network,
                        format!("the server answered {}", answer.response.status().as_u16()),
                    ));
                }
                let path = target.real().to_path_buf();
                let file = tokio::task::spawn_blocking(move || target.create())
                    .await
                    .map_err(|error| AlefError::new(ErrorCode::Internal, error.to_string()))??;
                let (writer, id) = ctx.streams().open_outgoing();
                tokio::spawn(fill(answer, file, path, writer));
                json(&json!({ "stream": id.0 }))
            }
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn progress_is_told_after_a_good_many_new_bytes() {
        let step = 256 * 1024;
        assert!(!progress_due(0, 0));
        assert!(!progress_due(step - 1, 0));
        assert!(progress_due(step, 0));
        assert!(progress_due(step * 5, 0));
        assert!(!progress_due(step + 99, step));
        assert!(!progress_due(step * 2 - 1, step));
        assert!(progress_due(step * 3, step * 2));
    }
}
