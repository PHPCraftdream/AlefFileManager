// SPDX-License-Identifier: MIT OR Apache-2.0
//! `GET stream/<id>` and the `runtime.stream.*` control commands.
use super::{
    auth,
    http::{self, FrameBody, TransportRequest, TransportResponse},
    Transport,
};
use crate::{
    error::{AlefError, ErrorCode},
    ids::StreamId,
    registry::{command::Reply, dispatch::Registry},
};
use serde::Deserialize;
use serde_json::json;

fn not_found() -> AlefError {
    AlefError::new(ErrorCode::NotFound, "stream not found")
}

impl Transport {
    /// Hands the outgoing frames of a stream to the glue; the reader can be taken only once.
    pub(super) fn stream(&self, id: &str, request: &TransportRequest) -> TransportResponse {
        let Some(session) = auth::session(&self.sessions, request) else {
            return http::error(&http::denied());
        };
        let id = match id.bytes().all(|b| b.is_ascii_digit()) {
            true => id.parse::<u64>().ok().map(StreamId),
            false => None,
        };
        match id.and_then(|id| session.streams().reader(id)) {
            Some(reader) => http::frames(FrameBody::new(reader)),
            None => http::error(&not_found()),
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AckArgs {
    id: StreamId,
    bytes: usize,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct StreamArgs {
    id: StreamId,
}

fn done() -> Result<Reply, AlefError> {
    Ok(Reply::Json(json!({})))
}

/// Registers `runtime.stream.{ack,close,write,end}`; they act on the caller's own session only.
pub(super) fn register_runtime_commands(registry: &mut Registry) -> Result<(), AlefError> {
    registry
        .register_runtime::<AckArgs>("runtime.stream.ack")?
        .handler(|ctx, args| async move {
            ctx.streams().ack(args.id, args.bytes)?;
            done()
        })?;
    registry
        .register_runtime::<StreamArgs>("runtime.stream.close")?
        .handler(|ctx, args| async move {
            ctx.streams().close(args.id)?;
            done()
        })?;
    registry
        .register_runtime::<StreamArgs>("runtime.stream.write")?
        .handler(|ctx, args| async move {
            let chunk = ctx
                .body()
                .cloned()
                .ok_or_else(|| AlefError::new(ErrorCode::InvalidArgument, "chunk body required"))?;
            let writer = ctx
                .streams()
                .incoming_writer(args.id)
                .ok_or_else(not_found)?;
            writer.write(chunk).await?;
            done()
        })?;
    registry
        .register_runtime::<StreamArgs>("runtime.stream.end")?
        .handler(|ctx, args| async move {
            let writer = ctx
                .streams()
                .incoming_writer(args.id)
                .ok_or_else(not_found)?;
            writer.end();
            done()
        })
}
