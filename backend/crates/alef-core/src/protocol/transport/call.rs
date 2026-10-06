// SPDX-License-Identifier: MIT OR Apache-2.0
//! Unary calls (`POST call/<command>`) and the `runtime.hello` handshake.
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
    session::Session,
};
use bytes::Bytes;
use serde_json::{json, Value};
use std::sync::Arc;
use tokio::task::JoinHandle;

/// Handshake protocol version.
const PROTOCOL: u32 = 1;

fn invalid(message: &str) -> AlefError {
    AlefError::new(ErrorCode::InvalidArgument, message)
}

/// Arguments and optional binary body of a unary request.
fn unary_args(
    request: &TransportRequest,
    limits: &Limits,
) -> Result<(Value, Option<Bytes>), AlefError> {
    let content_type = request.header("content-type").unwrap_or("");
    let media = content_type.split(';').next().unwrap_or("").trim();
    let json_body = media == "application/json" || (media.is_empty() && request.body.is_empty());
    if json_body {
        if request.body.len() > limits.max_unary_body {
            return Err(invalid("request body too large"));
        }
        if request.body.is_empty() {
            return Ok((json!({}), None));
        }
        let args = serde_json::from_slice(&request.body)
            .map_err(|_| invalid("request body is not valid JSON"))?;
        return Ok((args, None));
    }
    if media != "application/octet-stream" {
        return Err(invalid("unsupported content type"));
    }
    if request.body.len() > limits.max_bulk_body {
        return Err(invalid("request body too large"));
    }
    let args = match request.header("x-alef-args") {
        Some(header) => parse_args_header(header)?,
        None => json!({}),
    };
    Ok((args, Some(request.body.clone())))
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
        let (args, body) = match unary_args(request, &self.config.limits) {
            Ok(parts) => parts,
            Err(error) => return http::error(&error),
        };
        if name == "runtime.stream.write"
            && body
                .as_ref()
                .is_some_and(|b| b.len() > self.config.limits.chunk_size)
        {
            return http::error(&invalid("stream chunk too large"));
        }
        let _permit = match session.try_begin_call() {
            Ok(permit) => permit,
            Err(error) => return http::error(&error),
        };
        self.dispatch(session, name, args, body).await
    }

    /// Runs the handler in its own task so that a panic is an `INTERNAL` response.
    async fn dispatch(
        &self,
        session: Arc<Session>,
        name: &str,
        args: Value,
        body: Option<Bytes>,
    ) -> TransportResponse {
        let (cancel, handle) = CancelHandle::channel();
        let ctx = CallContext::new(session, self.permissions.clone())
            .with_body(body)
            .with_cancel(handle);
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
