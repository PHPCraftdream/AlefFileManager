// SPDX-License-Identifier: MIT OR Apache-2.0
//! Unary calls (`POST call/<command>`) and the `runtime.hello` handshake.
//!
//! The poll only does cheap checks (hello, session, name, call slot) and cheap parses; a large
//! JSON body is parsed on the bounded parse executor (still under the route watchdog kind) and the
//! handler runs in its own task guarded by `CallGuard`.
use super::{
    auth,
    http::{self, TransportRequest, TransportResponse},
    Transport,
};
use crate::{
    error::{AlefError, ErrorCode},
    protocol::call::{parse_args_header, Limits},
    registry::{
        command::Reply,
        context::{CallContext, CancelHandle, CancelSender},
        dispatch::valid_name,
    },
    session::{CallPermit, Session},
};
use bytes::Bytes;
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::task::JoinHandle;

/// Handshake protocol version.
const PROTOCOL: u32 = 1;

/// Commands that never wait and never hold anything, so they take no call slot: with every slot
/// taken by calls that wait, the page could not return credit (`ack`) and every outgoing stream
/// of the session would stop. `runtime.stream.write` waits for the reader and stays counted.
const UNCOUNTED: &[&str] = &[
    "runtime.stream.ack",
    "runtime.stream.close",
    "runtime.stream.end",
];

fn invalid(message: &str) -> AlefError {
    AlefError::new(ErrorCode::InvalidArgument, message)
}

/// Arguments and optional binary body of a unary request.
fn unary_args(
    content_type: &str,
    args_header: Option<&str>,
    body: Bytes,
    limits: &Limits,
) -> Result<(Value, Option<Bytes>), AlefError> {
    let media = content_type.split(';').next().unwrap_or("").trim();
    let json_body = media == "application/json" || (media.is_empty() && body.is_empty());
    if json_body {
        if body.len() > limits.max_unary_body {
            return Err(invalid("request body too large"));
        }
        if body.is_empty() {
            return Ok((json!({}), None));
        }
        let args =
            serde_json::from_slice(&body).map_err(|_| invalid("request body is not valid JSON"))?;
        return Ok((args, None));
    }
    if media != "application/octet-stream" {
        return Err(invalid("unsupported content type"));
    }
    if body.len() > limits.max_bulk_body {
        return Err(invalid("request body too large"));
    }
    let args = match args_header {
        Some(header) => parse_args_header(header)?,
        None => json!({}),
    };
    Ok((args, Some(body)))
}

/// JSON bodies above this size are parsed on the parse executor; smaller ones take microseconds.
pub(super) const INLINE_JSON_BODY: usize = 16 * 1024;

/// Whether the body is worth the parse executor: only a large JSON body costs real time.
fn parse_off_poll(content_type: &str, body: &Bytes) -> bool {
    let media = content_type.split(';').next().unwrap_or("").trim();
    media == "application/json" && body.len() > INLINE_JSON_BODY
}

/// A running handler task: dropping it (client abort) cancels the call cooperatively and aborts the task.
struct CallGuard {
    task: JoinHandle<Result<Reply, AlefError>>,
    cancel: CancelSender,
}

impl Drop for CallGuard {
    fn drop(&mut self) {
        self.cancel.cancel();
        self.task.abort();
    }
}

impl Transport {
    pub(super) async fn call(&self, name: &str, request: &TransportRequest) -> TransportResponse {
        if name == "runtime.hello" {
            return self.hello(request).await;
        }
        let Some(session) = auth::session(&self.sessions, request) else {
            return http::error(&http::denied());
        };
        if !valid_name(name) {
            return http::error(&AlefError::new(ErrorCode::NotFound, "command not found"));
        }
        let uncounted = UNCOUNTED.contains(&name);
        let media = request.header("content-type").unwrap_or("").to_owned();
        let args_header = request.header("x-alef-args").map(str::to_owned);
        let body = request.body.clone();
        let limits = self.config.limits;
        // A large JSON body is parsed on a bounded executor, off the poll and off the runtime
        // workers, still under the route watchdog: the await is inside the watched future. A full
        // parser refuses with `Busy`; dropping the call skips a queued parse and abandons a running
        // one (BlockingJob). Everything else (a binary body only splits off the args header) is
        // cheap and parsed in place, so ordinary calls and stream credit never meet `Busy`.
        let parsed = if parse_off_poll(&media, &body) {
            match self
                .parse
                .submit(move || unary_args(&media, args_header.as_deref(), body, &limits))
            {
                Ok(job) => match job.result().await {
                    Ok(parsed) => parsed,
                    Err(error) => Err(error),
                },
                Err(error) => Err(error),
            }
        } else {
            unary_args(&media, args_header.as_deref(), body, &limits)
        };
        let (args, body) = match parsed {
            Ok(parsed) => parsed,
            Err(error) => return http::error(&error),
        };
        if name == "runtime.stream.write"
            && body.as_ref().is_some_and(|b| b.len() > limits.chunk_size)
        {
            return http::error(&invalid("stream chunk too large"));
        }
        let permit = if uncounted {
            None
        } else {
            match session.try_begin_call() {
                Ok(permit) => Some(Arc::new(permit)),
                Err(error) => return http::error(&error),
            }
        };
        // The slot is held here until the response is ready: the context only borrows it for `waiting`.
        let response = self
            .dispatch(session, name, args, body, permit.clone())
            .await;
        drop(permit);
        response
    }

    /// Runs the handler in its own task so that a panic is an `INTERNAL` response.
    async fn dispatch(
        &self,
        session: Arc<Session>,
        name: &str,
        args: Value,
        body: Option<Bytes>,
        permit: Option<Arc<CallPermit>>,
    ) -> TransportResponse {
        let (cancel, handle) = CancelHandle::channel();
        let mut ctx = CallContext::new(session, self.permissions.clone())
            .with_body(body)
            .with_cancel(handle);
        if let Some(permit) = permit {
            ctx = ctx.with_permit(permit);
        }
        let registry = self.registry.clone();
        let name = name.to_owned();
        let task = tokio::spawn(async move { registry.dispatch(&name, ctx, args).await });
        let mut guard = CallGuard { task, cancel };
        match (&mut guard.task).await {
            Ok(Ok(Reply::Json(value))) => http::json(&value),
            Ok(Ok(Reply::Bytes(bytes))) => http::octets(bytes),
            Ok(Ok(Reply::Stream(id))) => http::json(&json!({ "stream": id })),
            Ok(Err(error)) => http::error(&error),
            Err(_) => http::error(&AlefError::new(ErrorCode::Internal, "internal error")),
        }
    }

    /// `runtime.hello`: bootstrap token + origin in, the calling document's session token out.
    /// With a known document the session is created (or rotated) here, so the handshake does not
    /// depend on the embedder having observed the navigation first.
    async fn hello(&self, request: &TransportRequest) -> TransportResponse {
        if !auth::bootstrap_ok(&self.config.bootstrap_token, request) {
            return http::error(&http::denied());
        }
        let session = match request.document {
            Some(document) => {
                match self
                    .sessions
                    .session_for_document(request.window, document)
                    .await
                {
                    Some(session) => session,
                    None => return http::error(&http::denied()),
                }
            }
            None => match self.sessions.current(request.window) {
                Some(session) => session,
                None => {
                    return http::error(&AlefError::new(ErrorCode::NotFound, "no document session"))
                }
            },
        };
        let mut modules: Vec<String> = self
            .registry
            .command_names()
            .iter()
            .filter_map(|name| name.split('.').next())
            .map(str::to_owned)
            .collect();
        modules.dedup();
        let limits = &self.config.limits;
        http::json(&json!({
            "protocol": PROTOCOL,
            "runtime": self.config.runtime_version,
            "platform": std::env::consts::OS,
            "arch": std::env::consts::ARCH,
            "modules": modules,
            "token": session.token(),
            "limits": {
                "maxUnaryBody": limits.max_unary_body,
                "maxBulkBody": limits.max_bulk_body,
                "streamWindow": limits.stream_window,
                "chunkSize": limits.chunk_size,
                "maxResources": limits.max_resources_per_session,
                "maxConcurrentCalls": limits.max_concurrent_calls_per_session,
            },
        }))
    }
}
