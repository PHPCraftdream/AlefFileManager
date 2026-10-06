// SPDX-License-Identifier: MIT OR Apache-2.0
//! Servo-free request/response layer of the native transport (`native://call`, `native://stream`).
//!
//! The glue turns a real HTTP request into a [`TransportRequest`], awaits [`Transport::handle`]
//! and streams the [`TransportResponse`] back. Everything here is plain async Rust.
mod auth;
mod call;
mod http;
mod stream;
#[cfg(test)]
mod tests;

pub use http::{FrameBody, Method, ResponseBody, TransportRequest, TransportResponse};

use crate::{
    error::{AlefError, ErrorCode},
    protocol::call::Limits,
    registry::dispatch::Registry,
    security::permissions::PermissionSet,
    session::session::SessionManager,
};
use std::{fmt, sync::Arc};

/// Transport policy.
pub struct TransportConfig {
    /// Per-runtime secret that only authorizes `runtime.hello`; never printed.
    pub bootstrap_token: String,
    /// Allowed `Origin` values; empty means any origin.
    pub allowed_origins: Vec<String>,
    /// Runtime version reported by `runtime.hello`.
    pub runtime_version: String,
    /// Per-session limits.
    pub limits: Limits,
}

impl fmt::Debug for TransportConfig {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TransportConfig")
            .field("bootstrap_token", &"<redacted>")
            .field("allowed_origins", &self.allowed_origins)
            .field("runtime_version", &self.runtime_version)
            .field("limits", &self.limits)
            .finish()
    }
}

/// Routes requests to the command registry and the session stream hubs.
pub struct Transport {
    config: TransportConfig,
    registry: Registry,
    sessions: Arc<SessionManager>,
    permissions: Arc<PermissionSet>,
}

enum Route<'a> {
    Call(&'a str),
    Stream(&'a str),
}

impl Transport {
    /// Builds the transport and registers the `runtime.stream.*` commands into `registry`.
    pub fn new(
        config: TransportConfig,
        mut registry: Registry,
        sessions: Arc<SessionManager>,
        permissions: Arc<PermissionSet>,
    ) -> Result<Self, AlefError> {
        stream::register_runtime_commands(&mut registry)?;
        Ok(Self {
            config,
            registry,
            sessions,
            permissions,
        })
    }

    /// Handles one request. Never panics and never fails: every problem is a response.
    pub async fn handle(&self, request: TransportRequest) -> TransportResponse {
        let origin = request.header("origin").map(str::to_owned);
        let allowed = auth::origin_allowed(&self.config.allowed_origins, origin.as_deref());
        let mut response = if allowed {
            self.route(&request).await
        } else {
            http::error(&http::denied())
        };
        let echoed = origin.as_deref().filter(|_| allowed);
        http::add_common_headers(&mut response, echoed);
        response
    }

    async fn route(&self, request: &TransportRequest) -> TransportResponse {
        let path = request.path.trim_start_matches('/');
        let route = if let Some(name) = path.strip_prefix("call/") {
            Route::Call(name)
        } else if let Some(id) = path.strip_prefix("stream/") {
            Route::Stream(id)
        } else {
            return http::error(&AlefError::new(ErrorCode::NotFound, "route not found"));
        };
        let method_ok = matches!(
            (&route, request.method),
            (_, Method::Options) | (Route::Call(_), Method::Post) | (Route::Stream(_), Method::Get)
        );
        if !method_ok {
            let error = AlefError::new(ErrorCode::InvalidArgument, "method not allowed");
            return http::with_status(405, &error);
        }
        if request.method == Method::Options {
            return http::preflight();
        }
        match route {
            Route::Call(name) => self.call(name, request).await,
            Route::Stream(id) => self.stream(id, request),
        }
    }
}
