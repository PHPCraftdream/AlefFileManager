// SPDX-License-Identifier: MIT OR Apache-2.0
//! Origin policy and bearer authentication.
use super::http::TransportRequest;
use crate::session::{
    session::{constant_time_eq, SessionManager},
    Session,
};
use std::sync::Arc;

/// With an empty allow-list any origin passes; otherwise the request must carry a listed one.
pub(super) fn origin_allowed(allowed: &[String], origin: Option<&str>) -> bool {
    allowed.is_empty() || origin.is_some_and(|origin| allowed.iter().any(|a| a == origin))
}

/// Token of an `Authorization: Bearer <token>` header.
fn bearer(request: &TransportRequest) -> Option<&str> {
    let value = request.header("authorization")?;
    let (scheme, token) = value.split_once(' ')?;
    (scheme.eq_ignore_ascii_case("bearer") && !token.is_empty()).then_some(token)
}

/// Whether the request presents the per-runtime bootstrap token (constant-time comparison).
pub(super) fn bootstrap_ok(bootstrap: &str, request: &TransportRequest) -> bool {
    !bootstrap.is_empty()
        && bearer(request)
            .is_some_and(|token| constant_time_eq(token.as_bytes(), bootstrap.as_bytes()))
}

/// Session of a valid bearer token, only when it belongs to the requesting window.
pub(super) fn session(
    sessions: &SessionManager,
    request: &TransportRequest,
) -> Option<Arc<Session>> {
    let session = sessions.validate(bearer(request)?)?;
    (session.window() == request.window).then_some(session)
}
