// SPDX-License-Identifier: MIT OR Apache-2.0
//! The `media.camera` commands: listing the cameras of the machine, opening one onto a stream the
//! page reads, and closing it. Opening needs the `media.camera` permission; a substituted right is
//! answered `NotAvailable` without touching a device.
use std::{future::Future, pin::Pin, sync::Arc};

use alef_core::{
    ids::ResourceId, registry::dispatch::Registry, security::permissions::Permission,
    session::resources::Resource, AlefError,
};
use serde::Deserialize;
use serde_json::json;

use crate::{json, media::invalid};

use super::{
    backend::{CameraBackend, DeviceStream},
    queue::CameraFrame,
    substituted,
};

/// A camera the page opened; closing it (explicitly, or at the teardown of the session) cancels the
/// relay, waits for its end and for the producer's blocking job.
struct CameraHandle {
    stream: Arc<DeviceStream<CameraFrame>>,
    relay: super::RelayHandle,
}

impl Resource for CameraHandle {
    fn close(self: Box<Self>) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        Box::pin(async move {
            self.relay.shutdown(&self.stream).await;
            if let Some(job) = self.stream.take_job() {
                let _ = job.result().await;
            }
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct OpenArgs {
    id: String,
    #[serde(default)]
    fps: Option<f64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CloseArgs {
    camera: u64,
}

pub(crate) fn register(
    registry: &mut Registry,
    backend: Arc<dyn CameraBackend>,
) -> Result<(), AlefError> {
    let list = backend.clone();
    registry
        .command::<()>("media.camera.list")?
        .permission(Permission::MediaCamera, |_| None)
        .substitutes()
        .handler(move |ctx, ()| {
            let backend = list.clone();
            async move {
                substituted(&ctx, "media.camera")?;
                let devices = crate::media::executor()
                    .submit(move || backend.list())?
                    .result()
                    .await??;
                json(&json!({ "devices": devices }))
            }
        })?;
    let open = backend.clone();
    registry
        .command::<OpenArgs>("media.camera.open")?
        .permission(Permission::MediaCamera, |_| None)
        .substitutes()
        .handler(move |ctx, args| {
            let backend = open.clone();
            async move {
                substituted(&ctx, "media.camera")?;
                let fps = args.fps.unwrap_or(30.0);
                if !fps.is_finite() || !(1.0..=120.0).contains(&fps) {
                    return Err(invalid("fps must be a number between 1 and 120".to_owned()));
                }
                let id = args.id.clone();
                let stream = crate::media::executor()
                    .submit(move || backend.open(&id, fps))?
                    .result()
                    .await??;
                let (writer, stream_id) = ctx.streams().open_outgoing();
                let relay = super::RelayHandle::spawn(writer, stream.clone());
                let camera = match ctx.resources().insert(Box::new(CameraHandle {
                    stream: stream.clone(),
                    relay,
                })) {
                    Ok(camera) => camera,
                    // Without the resource nobody would ever stop the producer.
                    Err(error) => {
                        stream.stop();
                        return Err(error);
                    }
                };
                json(&json!({ "camera": camera.0, "stream": stream_id.0 }))
            }
        })?;
    registry
        .command::<CloseArgs>("media.camera.close")?
        .handler(move |ctx, args| async move {
            let id = ResourceId(args.camera);
            // An unknown or foreign id is NOT_FOUND and the resource is left alone.
            let (frames, dropped) = ctx
                .resources()
                .with_as::<CameraHandle, _>(id, |handle| handle.stream.stats())?;
            if let Ok(resource) = ctx.resources().take(id) {
                resource.close().await;
            }
            json(&json!({
                "camera": args.camera,
                "frames": frames,
                "dropped": dropped,
            }))
        })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::virtuals::VirtualCamera;
    use alef_core::{
        ids::StreamId,
        protocol::{call::Limits, frame::Frame},
        registry::{command::Reply, context::CallContext},
        security::{
            consent::{Consent, Decision, Right},
            manifest,
            permissions::{PathVars, PermissionSet},
        },
        session::{session::Session, session::SessionManager, streams::StreamReader},
        ErrorCode,
    };
    use std::time::Duration;

    const WAIT: Duration = Duration::from_secs(10);

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

    fn permissions(media: Option<(bool, bool)>, audio: bool) -> Arc<PermissionSet> {
        let (camera, microphone) = media.unwrap_or((false, false));
        let policy: manifest::Permissions = serde_json::from_value(json!({
            "fs": {"read": [], "write": []},
            "cli": {},
            "net": {"http": [], "socket": []},
            "shell": {"openExternal": []},
            "clipboard": {"read": false},
            "shortcut": {"global": false},
            "secrets": false,
            "app": {"env": []},
            "media": {"camera": camera, "microphone": microphone},
            "audio": audio,
        }))
        .unwrap();
        Arc::new(
            PermissionSet::from_manifest(&policy, &path_vars())
                .unwrap()
                .with_consent(Consent::allow_all()),
        )
    }

    async fn context_with(media: Option<(bool, bool)>, audio: bool) -> CallContext {
        CallContext::new(session().await, permissions(media, audio))
    }

    /// Permissions whose media rights come from a consent substitution, so the registry recomputes
    /// `Decision::Substitute` from the permission set itself.
    fn substituted_permissions(media: Option<(bool, bool)>, audio: bool) -> Arc<PermissionSet> {
        let (camera, microphone) = media.unwrap_or((false, false));
        let policy: manifest::Permissions = serde_json::from_value(json!({
            "fs": {"read": [], "write": []},
            "cli": {},
            "net": {"http": [], "socket": []},
            "shell": {"openExternal": []},
            "clipboard": {"read": false},
            "shortcut": {"global": false},
            "secrets": false,
            "app": {"env": []},
            "media": {"camera": camera, "microphone": microphone},
            "audio": audio,
        }))
        .unwrap();
        let mut consent = Consent::undecided();
        consent.set(Right::plain("media.camera"), Decision::Substitute);
        consent.set(Right::plain("media.microphone"), Decision::Substitute);
        consent.set(Right::plain("audio"), Decision::Substitute);
        Arc::new(
            PermissionSet::from_manifest(&policy, &path_vars())
                .unwrap()
                .with_consent(consent),
        )
    }

    fn registry() -> Registry {
        let mut registry = Registry::default();
        register(&mut registry, Arc::new(VirtualCamera::default())).unwrap();
        registry
    }

    async fn active_streams(camera: &Arc<VirtualCamera>, expected: u64) {
        tokio::time::timeout(Duration::from_secs(5), async {
            while camera.active_streams() != expected {
                tokio::time::sleep(Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the streams must settle in time");
    }

    #[tokio::test]
    async fn a_substituted_camera_answers_not_available_without_opening_a_device() {
        let camera = Arc::new(VirtualCamera::default());
        let mut registry = Registry::default();
        register(&mut registry, camera.clone()).unwrap();
        let ctx = CallContext::new(
            session().await,
            substituted_permissions(Some((true, true)), true),
        );
        let error = registry
            .dispatch("media.camera.list", ctx.clone(), serde_json::Value::Null)
            .await
            .expect_err("a substituted list must be refused");
        assert_eq!(error.code, ErrorCode::NotAvailable);
        let error = registry
            .dispatch(
                "media.camera.open",
                ctx,
                json!({"id": "virtual-0", "fps": 30}),
            )
            .await
            .expect_err("a substituted open must be refused");
        assert_eq!(error.code, ErrorCode::NotAvailable);
        assert_eq!(camera.active_streams(), 0);
    }

    #[tokio::test]
    async fn an_undeclared_media_camera_right_is_permission_denied() {
        let registry = registry();
        let ctx = context_with(None, false).await;
        let error = registry
            .dispatch("media.camera.list", ctx, serde_json::Value::Null)
            .await
            .expect_err("a closed camera must be refused");
        assert_eq!(error.code, ErrorCode::PermissionDenied);
    }

    #[tokio::test]
    async fn an_open_camera_streams_full_frames_and_close_stops_the_producer() {
        let camera = Arc::new(VirtualCamera::default());
        let mut registry = Registry::default();
        register(&mut registry, camera.clone()).unwrap();
        let ctx = context_with(Some((true, true)), true).await;
        let reply = registry
            .dispatch("media.camera.open", ctx.clone(), json!({"id": "virtual-0"}))
            .await
            .expect("the open must succeed");
        let Reply::Json(reply) = reply else {
            panic!("expected a JSON reply");
        };
        let mut reader: StreamReader = ctx
            .streams()
            .reader(StreamId(reply["stream"].as_u64().unwrap()))
            .unwrap();
        let header = tokio::time::timeout(WAIT, reader.next_frame())
            .await
            .unwrap()
            .unwrap();
        let Frame::Json(header) = header else {
            panic!("expected a JSON header first");
        };
        assert_eq!(header["width"], 1280);
        let byte_length = header["byteLength"].as_u64().unwrap();
        let mut assembled = Vec::new();
        while assembled.len() != byte_length as usize {
            let frame = tokio::time::timeout(WAIT, reader.next_frame())
                .await
                .unwrap()
                .unwrap();
            match frame {
                Frame::Binary(piece) => {
                    ctx.streams()
                        .ack(StreamId(reply["stream"].as_u64().unwrap()), piece.len())
                        .unwrap();
                    assembled.extend_from_slice(&piece);
                }
                Frame::Json(_) => panic!("a new header arrived inside a payload"),
                other => panic!("unexpected frame {other:?}"),
            }
        }
        assert_eq!(assembled.len(), 1280 * 720 * 4);
        assert_eq!(&assembled[..8], &[3, 5, 11, 255, 4, 5, 11, 255]);
        let close = registry
            .dispatch(
                "media.camera.close",
                ctx.clone(),
                json!({"camera": reply["camera"]}),
            )
            .await
            .expect("the close must succeed");
        let Reply::Json(close) = close else {
            panic!("expected a JSON close reply");
        };
        assert!(close["frames"].as_u64().unwrap() >= 1);
        let end = tokio::time::timeout(WAIT, reader.next_frame())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(end, Frame::End));
        active_streams(&camera, 0).await;
    }

    #[tokio::test]
    async fn closing_a_camera_while_the_relay_is_stalled_on_credit_stops_everything() {
        let camera = Arc::new(VirtualCamera::default());
        let mut registry = Registry::default();
        register(&mut registry, camera.clone()).unwrap();
        let ctx = context_with(Some((true, true)), true).await;
        let reply = registry
            .dispatch("media.camera.open", ctx.clone(), json!({"id": "virtual-0"}))
            .await
            .expect("the open must succeed");
        let Reply::Json(reply) = reply else {
            panic!("expected a JSON reply");
        };
        let mut reader: StreamReader = ctx
            .streams()
            .reader(StreamId(reply["stream"].as_u64().unwrap()))
            .unwrap();
        let header = tokio::time::timeout(WAIT, reader.next_frame())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(header, Frame::Json(_)));
        // No acks: the relay blocks on credit until the close wakes it.
        tokio::time::timeout(
            WAIT,
            registry.dispatch(
                "media.camera.close",
                ctx.clone(),
                json!({"camera": reply["camera"]}),
            ),
        )
        .await
        .expect("the close must not hang")
        .expect("the close must succeed");
        let end = tokio::time::timeout(WAIT, reader.next_frame())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(end, Frame::End));
        active_streams(&camera, 0).await;
    }

    #[tokio::test]
    async fn an_opened_camera_that_was_never_iterated_closes_cleanly() {
        let camera = Arc::new(VirtualCamera::default());
        let mut registry = Registry::default();
        register(&mut registry, camera.clone()).unwrap();
        let ctx = context_with(Some((true, true)), true).await;
        let reply = registry
            .dispatch("media.camera.open", ctx.clone(), json!({"id": "virtual-0"}))
            .await
            .expect("the open must succeed");
        let Reply::Json(reply) = reply else {
            panic!("expected a JSON reply");
        };
        let mut reader: StreamReader = ctx
            .streams()
            .reader(StreamId(reply["stream"].as_u64().unwrap()))
            .unwrap();
        tokio::time::timeout(
            WAIT,
            registry.dispatch(
                "media.camera.close",
                ctx.clone(),
                json!({"camera": reply["camera"]}),
            ),
        )
        .await
        .expect("the close must not hang")
        .expect("the close must succeed");
        let end = tokio::time::timeout(WAIT, reader.next_frame())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(end, Frame::End));
        active_streams(&camera, 0).await;
    }

    #[tokio::test]
    async fn a_camera_close_between_frames_does_not_wait_for_the_next_frame() {
        let camera = Arc::new(VirtualCamera::default());
        let mut registry = Registry::default();
        register(&mut registry, camera.clone()).unwrap();
        let ctx = context_with(Some((true, true)), true).await;
        let reply = registry
            .dispatch(
                "media.camera.open",
                ctx.clone(),
                json!({"id": "virtual-0", "fps": 1}),
            )
            .await
            .expect("the open must succeed");
        let Reply::Json(reply) = reply else {
            panic!("expected a JSON reply");
        };
        let mut reader: StreamReader = ctx
            .streams()
            .reader(StreamId(reply["stream"].as_u64().unwrap()))
            .unwrap();
        let header = tokio::time::timeout(WAIT, reader.next_frame())
            .await
            .unwrap()
            .unwrap();
        let Frame::Json(header) = header else {
            panic!("expected a JSON header first");
        };
        let byte_length = header["byteLength"].as_u64().unwrap();
        let mut assembled = Vec::new();
        while assembled.len() != byte_length as usize {
            let frame = tokio::time::timeout(WAIT, reader.next_frame())
                .await
                .unwrap()
                .unwrap();
            match frame {
                Frame::Binary(piece) => {
                    ctx.streams()
                        .ack(StreamId(reply["stream"].as_u64().unwrap()), piece.len())
                        .unwrap();
                    assembled.extend_from_slice(&piece);
                }
                Frame::Json(_) => panic!("a new header arrived inside a payload"),
                other => panic!("unexpected frame {other:?}"),
            }
        }
        // The producer now sleeps for about a second, so the close lands between frames; a close
        // that waited for the next frame would take over a second.
        tokio::time::timeout(
            Duration::from_millis(500),
            registry.dispatch(
                "media.camera.close",
                ctx.clone(),
                json!({"camera": reply["camera"]}),
            ),
        )
        .await
        .expect("the close must return between frames")
        .expect("the close must succeed");
        let end = tokio::time::timeout(WAIT, reader.next_frame())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(end, Frame::End));
        active_streams(&camera, 0).await;
        tokio::time::timeout(WAIT, ctx.session.close())
            .await
            .expect("the session must close in time");
    }

    #[tokio::test]
    async fn a_close_for_a_foreign_resource_kind_is_refused_and_leaves_the_camera_open() {
        let camera = Arc::new(VirtualCamera::default());
        let mut registry = Registry::default();
        register(&mut registry, camera.clone()).unwrap();
        crate::media::microphone::register(
            &mut registry,
            Arc::new(crate::media::virtuals::VirtualMicrophone::default()),
        )
        .unwrap();
        let ctx = context_with(Some((true, true)), true).await;
        let reply = registry
            .dispatch("media.camera.open", ctx.clone(), json!({"id": "virtual-0"}))
            .await
            .expect("the open must succeed");
        let Reply::Json(reply) = reply else {
            panic!("expected a JSON reply");
        };
        let error = registry
            .dispatch(
                "media.microphone.close",
                ctx.clone(),
                json!({"microphone": reply["camera"]}),
            )
            .await
            .expect_err("a camera id is not a microphone");
        assert_eq!(error.code, ErrorCode::NotFound);
        let close = registry
            .dispatch(
                "media.camera.close",
                ctx.clone(),
                json!({"camera": reply["camera"]}),
            )
            .await
            .expect("the close must succeed");
        let Reply::Json(close) = close else {
            panic!("expected a JSON close reply");
        };
        assert!(close["frames"].as_u64().unwrap() >= 1);
        tokio::time::timeout(WAIT, ctx.session.close())
            .await
            .expect("the session must close in time");
    }

    #[tokio::test]
    async fn closing_the_session_stops_the_camera_producer() {
        let camera = Arc::new(VirtualCamera::default());
        let mut registry = Registry::default();
        register(&mut registry, camera.clone()).unwrap();
        let ctx = context_with(Some((true, true)), true).await;
        registry
            .dispatch("media.camera.open", ctx.clone(), json!({"id": "virtual-0"}))
            .await
            .expect("the open must succeed");
        tokio::time::timeout(WAIT, ctx.session.close())
            .await
            .expect("the session must close in time");
        active_streams(&camera, 0).await;
    }
}
