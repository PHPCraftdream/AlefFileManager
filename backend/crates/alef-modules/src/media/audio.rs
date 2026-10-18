// SPDX-License-Identifier: MIT OR Apache-2.0
//! The `audio` commands: opening a sink the page writes its samples to. Opening needs the `audio`
//! permission; the samples arrive as the body of `audio.write` and are only counted, never kept.
use std::{future::Future, pin::Pin, sync::Arc, sync::Mutex};

use alef_core::{
    ids::{ResourceId, StreamId},
    registry::dispatch::Registry,
    security::permissions::Permission,
    session::resources::Resource,
    AlefError, ErrorCode,
};
use serde::Deserialize;
use serde_json::json;

use crate::{json, media::invalid};

use super::{backend::AudioBackend, substituted};

/// A sink the page opened; closing it (explicitly, or at the teardown of the session) terminates
/// the incoming stream first, drains what is already queued, then closes the sink, so a later
/// write answers `Closed`.
struct AudioSinkHandle {
    sink: super::backend::AudioSink,
    stream_id: StreamId,
    done_rx: Mutex<Option<tokio::sync::oneshot::Receiver<()>>>,
}

impl Resource for AudioSinkHandle {
    fn close(self: Box<Self>) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        self.sink.close();
        Box::pin(async {})
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteArgs {
    sink: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CloseArgs {
    sink: u64,
}

pub(crate) fn register(
    registry: &mut Registry,
    backend: Arc<dyn AudioBackend>,
) -> Result<(), AlefError> {
    let open = backend.clone();
    registry
        .command::<()>("audio.open")?
        .permission(Permission::Audio, |_| None)
        .substitutes()
        .handler(move |ctx, ()| {
            let backend = open.clone();
            async move {
                substituted(&ctx, "audio")?;
                let sink = backend.open()?;
                let (mut reader, stream_id) = ctx.streams().open_incoming_reader();
                let (done_tx, done_rx) = tokio::sync::oneshot::channel();
                let sink_id = ctx.resources().insert(Box::new(AudioSinkHandle {
                    sink: sink.clone(),
                    stream_id,
                    done_rx: Mutex::new(Some(done_rx)),
                }))?;
                // The page writes into the incoming stream; the sink takes every chunk until the
                // stream is over or the sink refuses, and it never stays blocked after a close,
                // because close terminates the stream.
                tokio::spawn(async move {
                    while let Some(Ok(chunk)) = reader.recv().await {
                        if sink.write(&chunk).is_err() {
                            break;
                        }
                    }
                    let _ = done_tx.send(());
                });
                json(&json!({ "sink": sink_id.0, "stream": stream_id.0 }))
            }
        })?;
    registry
        .command::<WriteArgs>("audio.write")?
        .handler(move |ctx, args| async move {
            let body = ctx
                .body()
                .ok_or_else(|| invalid("audio.write needs its samples as the body".to_owned()))?
                .clone();
            let sink = ctx
                .resources()
                .with_as::<AudioSinkHandle, _>(ResourceId(args.sink), |handle| handle.sink.clone())
                .map_err(|_| {
                    AlefError::new(ErrorCode::Closed, "the audio sink is closed already")
                })?;
            let written = crate::media::executor()
                .submit(move || sink.write(&body))?
                .result()
                .await??;
            json(&json!({ "sink": args.sink, "samples": written }))
        })?;
    registry
        .command::<CloseArgs>("audio.close")?
        .handler(move |ctx, args| async move {
            let id = ResourceId(args.sink);
            // Close drains the queued writes first and refuses unknown or foreign ids with
            // NOT_FOUND, leaving the resource alone.
            let (sink, stream_id, done) =
                ctx.resources()
                    .with_as::<AudioSinkHandle, _>(id, |handle| {
                        (
                            handle.sink.clone(),
                            handle.stream_id,
                            handle
                                .done_rx
                                .lock()
                                .unwrap_or_else(|e| e.into_inner())
                                .take(),
                        )
                    })?;
            // Terminate the input; queued chunks stay readable, so the consumer drains them.
            ctx.streams().close(stream_id)?;
            if let Some(done) = done {
                let _ = tokio::time::timeout(std::time::Duration::from_secs(5), done).await;
            }
            if let Ok(resource) = ctx.resources().take(id) {
                resource.close().await;
            }
            let (samples, checksum) = sink.close();
            // The checksum goes out as hex: a u64 does not survive a JSON number exactly.
            json(&json!({
                "sink": args.sink,
                "samples": samples,
                "checksum": super::backend::AudioSink::checksum_hex(checksum),
            }))
        })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::virtuals::VirtualAudio;
    use alef_core::{
        protocol::call::Limits,
        registry::{command::Reply, context::CallContext},
        security::{
            consent::Consent,
            manifest,
            permissions::{PathVars, PermissionSet},
        },
        session::{session::Session, session::SessionManager},
    };
    use bytes::Bytes;

    const WAIT: std::time::Duration = std::time::Duration::from_secs(10);

    async fn session() -> Arc<Session> {
        SessionManager::new(Arc::new(|| "test-token".to_owned()), Limits::default())
            .begin_document(1)
            .await
    }

    fn path_vars() -> PathVars {
        let path = std::env::temp_dir();
        PathVars {
            app_data: path.clone(),
            app_config: path.clone(),
            app_cache: path.clone(),
            home: path.clone(),
            documents: path.clone(),
            downloads: path.clone(),
            desktop: path.clone(),
            temp: path.clone(),
            app: Some(path),
        }
    }

    fn permissions(audio: bool) -> Arc<PermissionSet> {
        let policy: manifest::Permissions = serde_json::from_value(json!({
            "fs": {"read": [], "write": []},
            "cli": {},
            "net": {"http": [], "socket": []},
            "shell": {"openExternal": []},
            "clipboard": {"read": false},
            "shortcut": {"global": false},
            "secrets": false,
            "app": {"env": []},
            "audio": audio,
        }))
        .unwrap();
        Arc::new(
            PermissionSet::from_manifest(&policy, &path_vars())
                .unwrap()
                .with_consent(Consent::allow_all()),
        )
    }

    async fn opened() -> (Registry, CallContext, u64) {
        let mut registry = Registry::default();
        register(&mut registry, Arc::new(VirtualAudio)).unwrap();
        let ctx = CallContext::new(session().await, permissions(true));
        let reply = registry
            .dispatch("audio.open", ctx.clone(), serde_json::Value::Null)
            .await
            .expect("the open must succeed");
        let Reply::Json(reply) = reply else {
            panic!("expected a JSON reply");
        };
        (registry, ctx, reply["sink"].as_u64().unwrap())
    }

    fn fnv(bytes: &[u8]) -> u64 {
        let mut checksum = 0xcbf2_9ce4_8422_2325;
        for byte in bytes {
            checksum ^= u64::from(*byte);
            checksum = checksum.wrapping_mul(0x0000_0100_0000_01b3);
        }
        checksum
    }

    #[tokio::test]
    async fn written_samples_are_counted_and_the_checksum_is_reported_as_hex() {
        let (registry, ctx, sink) = opened().await;
        let first = Bytes::from(vec![0u8, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]);
        let reply = registry
            .dispatch(
                "audio.write",
                ctx.clone().with_body(Some(first.clone())),
                json!({"sink": sink}),
            )
            .await
            .expect("the first write must succeed");
        let Reply::Json(reply) = reply else {
            panic!("expected a JSON reply");
        };
        assert_eq!(reply["samples"], 4);
        let invalid = registry
            .dispatch(
                "audio.write",
                ctx.clone().with_body(Some(Bytes::from_static(&[1, 2, 3]))),
                json!({"sink": sink}),
            )
            .await
            .expect_err("a 3-byte body must be refused");
        assert_eq!(invalid.code, ErrorCode::InvalidArgument);
        let second = Bytes::from(vec![16u8, 17, 18, 19, 20, 21, 22, 23]);
        let reply = registry
            .dispatch(
                "audio.write",
                ctx.clone().with_body(Some(second.clone())),
                json!({"sink": sink}),
            )
            .await
            .expect("the second write must succeed");
        let Reply::Json(reply) = reply else {
            panic!("expected a JSON reply");
        };
        assert_eq!(reply["samples"], 6);
        let mut heard = first.to_vec();
        heard.extend_from_slice(&second);
        let reply = registry
            .dispatch("audio.close", ctx.clone(), json!({"sink": sink}))
            .await
            .expect("the close must succeed");
        let Reply::Json(reply) = reply else {
            panic!("expected a JSON close reply");
        };
        assert_eq!(
            reply["checksum"].as_str().unwrap(),
            crate::media::backend::AudioSink::checksum_hex(fnv(&heard))
        );
        let error = registry
            .dispatch(
                "audio.write",
                ctx.clone().with_body(Some(first)),
                json!({"sink": sink}),
            )
            .await
            .expect_err("a closed sink must refuse");
        assert_eq!(error.code, ErrorCode::Closed);
        tokio::time::timeout(WAIT, ctx.session.close())
            .await
            .expect("the session must close in time");
    }

    #[tokio::test]
    async fn audio_close_drains_the_writes_queued_before_it() {
        let (registry, ctx, _sink) = opened().await;
        let reply = registry
            .dispatch("audio.open", ctx.clone(), serde_json::Value::Null)
            .await
            .expect("the second open must succeed");
        let Reply::Json(reply) = reply else {
            panic!("expected a JSON reply");
        };
        let stream = reply["stream"].as_u64().unwrap();
        let writer = ctx
            .streams()
            .incoming_writer(StreamId(stream))
            .expect("the stream must be there");
        let mut queued = Vec::new();
        for first in 0u8..8 {
            let chunk = vec![
                first,
                first.wrapping_add(1),
                first.wrapping_add(2),
                first.wrapping_add(3),
            ];
            queued.extend_from_slice(&chunk);
            tokio::time::timeout(WAIT, writer.write(Bytes::from(chunk)))
                .await
                .expect("the write must go through in time")
                .expect("the write must succeed");
        }
        let reply = registry
            .dispatch(
                "audio.close",
                ctx.clone(),
                json!({"sink": reply["sink"].as_u64().unwrap()}),
            )
            .await
            .expect("the close must succeed");
        let Reply::Json(reply) = reply else {
            panic!("expected a JSON close reply");
        };
        assert_eq!(reply["samples"], 8);
        assert_eq!(
            reply["checksum"].as_str().unwrap(),
            crate::media::backend::AudioSink::checksum_hex(fnv(&queued))
        );
        tokio::time::timeout(WAIT, ctx.session.close())
            .await
            .expect("the session must close in time");
    }

    #[tokio::test]
    async fn writing_after_an_explicit_close_is_refused() {
        let (registry, ctx, sink) = opened().await;
        let reply = registry
            .dispatch("audio.close", ctx.clone(), json!({"sink": sink}))
            .await
            .expect("the close must succeed");
        let Reply::Json(reply) = reply else {
            panic!("expected a JSON close reply");
        };
        assert_eq!(reply["samples"], 0);
        let error = registry
            .dispatch(
                "audio.write",
                ctx.clone()
                    .with_body(Some(Bytes::from_static(&[0, 0, 0, 0]))),
                json!({"sink": sink}),
            )
            .await
            .expect_err("a closed sink must refuse");
        assert_eq!(error.code, ErrorCode::Closed);
        tokio::time::timeout(WAIT, ctx.session.close())
            .await
            .expect("the session must close in time");
    }

    #[tokio::test]
    async fn writing_to_an_unknown_sink_is_closed() {
        let (registry, ctx, _sink) = opened().await;
        let error = registry
            .dispatch(
                "audio.write",
                ctx.clone()
                    .with_body(Some(Bytes::from_static(&[0, 0, 0, 0]))),
                json!({"sink": 999}),
            )
            .await
            .expect_err("an unknown sink must refuse");
        assert_eq!(error.code, ErrorCode::Closed);
        tokio::time::timeout(WAIT, ctx.session.close())
            .await
            .expect("the session must close in time");
    }
}
