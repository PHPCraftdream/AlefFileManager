// SPDX-License-Identifier: MIT OR Apache-2.0
//! The `media.microphone` commands: listing the microphones of the machine, opening one onto a
//! stream of `f32` chunks the page reads, and closing it. Opening needs the `media.microphone`
//! permission; a substituted right is answered `NotAvailable` without touching a device.
use std::{future::Future, pin::Pin, sync::Arc};

use alef_core::{
    ids::ResourceId, registry::dispatch::Registry, security::permissions::Permission,
    session::resources::Resource, AlefError,
};
use serde::Deserialize;
use serde_json::json;

use crate::{json, media::invalid};

use super::{
    backend::{DeviceStream, MicrophoneBackend},
    queue::PcmChunk,
    substituted,
};

/// A microphone the page opened; closing it (explicitly, or at the teardown of the session) stops
/// the producer and waits for its thread. The rate is kept for the statistics of the close.
struct MicrophoneHandle {
    stream: Arc<DeviceStream<PcmChunk>>,
    rate: u32,
}

impl Resource for MicrophoneHandle {
    fn close(self: Box<Self>) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        let stream = self.stream;
        Box::pin(async move {
            stream.stop();
            if let Some(job) = stream.take_job() {
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
    frequency: Option<f64>,
    #[serde(default, rename = "rate")]
    rate: Option<u32>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CloseArgs {
    microphone: u64,
}

pub(crate) fn register(
    registry: &mut Registry,
    backend: Arc<dyn MicrophoneBackend>,
) -> Result<(), AlefError> {
    let list = backend.clone();
    registry
        .command::<()>("media.microphone.list")?
        .permission(Permission::MediaMicrophone, |_| None)
        .substitutes()
        .handler(move |ctx, ()| {
            let backend = list.clone();
            async move {
                substituted(&ctx, "media.microphone")?;
                let devices = crate::media::executor()
                    .submit(move || backend.list())?
                    .result()
                    .await??;
                json(&json!({ "devices": devices }))
            }
        })?;
    let open = backend.clone();
    registry
        .command::<OpenArgs>("media.microphone.open")?
        .permission(Permission::MediaMicrophone, |_| None)
        .substitutes()
        .handler(move |ctx, args| {
            let backend = open.clone();
            async move {
                substituted(&ctx, "media.microphone")?;
                let frequency = args.frequency.unwrap_or(440.0);
                if !frequency.is_finite() || !(1.0..=20000.0).contains(&frequency) {
                    return Err(invalid(
                        "frequency must be a number between 1 and 20000".to_owned(),
                    ));
                }
                let rate = args.rate.unwrap_or(48000);
                if !(8000..=192000).contains(&rate) {
                    return Err(invalid("rate must be between 8000 and 192000".to_owned()));
                }
                let id = args.id.clone();
                let stream = crate::media::executor()
                    .submit(move || backend.open(&id, frequency, rate))?
                    .result()
                    .await??;
                let (writer, stream_id) = ctx.streams().open_outgoing();
                let microphone = match ctx.resources().insert(Box::new(MicrophoneHandle {
                    stream: stream.clone(),
                    rate,
                })) {
                    Ok(microphone) => microphone,
                    // Without the resource nobody would ever stop the producer.
                    Err(error) => {
                        stream.stop();
                        return Err(error);
                    }
                };
                tokio::spawn(super::relay(writer, stream));
                json(&json!({ "microphone": microphone.0, "stream": stream_id.0 }))
            }
        })?;
    registry
        .command::<CloseArgs>("media.microphone.close")?
        .handler(move |ctx, args| async move {
            let id = ResourceId(args.microphone);
            // An unknown or foreign id is NOT_FOUND and the resource is left alone.
            let (rate, (pushed, dropped)) = ctx
                .resources()
                .with_as::<MicrophoneHandle, _>(id, |handle| {
                    (handle.rate, handle.stream.stats())
                })?;
            if let Ok(resource) = ctx.resources().take(id) {
                resource.close().await;
            }
            json(&json!({
                "microphone": args.microphone,
                "chunks": pushed,
                "samples": pushed * u64::from(rate) * 20 / 1000,
                "dropped": dropped,
            }))
        })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::media::{
        audio,
        virtuals::{VirtualAudio, VirtualCamera, VirtualMicrophone},
    };
    use alef_core::{
        ids::StreamId,
        protocol::{call::Limits, frame::Frame},
        registry::{command::Reply, context::CallContext},
        security::{
            consent::{Consent, Decision, Right},
            manifest,
            permissions::{PathVars, PermissionSet},
        },
        session::{session::Session, session::SessionManager},
        ErrorCode,
    };

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

    /// Permissions whose media rights come from a consent substitution, like the camera tests use.
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

    fn registry_with(backend: Arc<VirtualMicrophone>) -> Registry {
        let mut registry = Registry::default();
        crate::media::camera::register(&mut registry, Arc::new(VirtualCamera::default())).unwrap();
        register(&mut registry, backend).unwrap();
        audio::register(&mut registry, Arc::new(VirtualAudio)).unwrap();
        registry
    }

    fn registry() -> Registry {
        registry_with(Arc::new(VirtualMicrophone::default()))
    }

    #[tokio::test]
    async fn a_substituted_microphone_and_audio_sink_answer_not_available() {
        let microphone = Arc::new(VirtualMicrophone::default());
        let registry = registry_with(microphone.clone());
        let ctx = CallContext::new(
            session().await,
            substituted_permissions(Some((true, true)), true),
        );
        for (name, args) in [
            ("media.microphone.list", serde_json::Value::Null),
            ("media.microphone.open", json!({"id": "virtual-0"})),
            ("audio.open", serde_json::Value::Null),
        ] {
            let error = registry
                .dispatch(name, ctx.clone(), args)
                .await
                .err()
                .unwrap_or_else(|| panic!("{name} must be refused"));
            assert_eq!(error.code, ErrorCode::NotAvailable, "{name}");
        }
        assert_eq!(microphone.active_streams(), 0);
    }

    #[tokio::test]
    async fn an_open_microphone_streams_pcm_chunks_of_the_declared_shape() {
        let registry = registry();
        let ctx = CallContext::new(session().await, permissions(Some((true, true)), true));
        let reply = registry
            .dispatch(
                "media.microphone.open",
                ctx.clone(),
                json!({"id": "virtual-0", "frequency": 440}),
            )
            .await
            .expect("the open must succeed");
        let Reply::Json(reply) = reply else {
            panic!("expected a JSON reply");
        };
        let mut reader = ctx
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
        assert_eq!(header["rate"], 48000);
        assert_eq!(header["samples"], 960);
        assert_eq!(header["format"], "f32le");
        assert_eq!(header["byteLength"], 3840);
        let payload = tokio::time::timeout(WAIT, reader.next_frame())
            .await
            .unwrap()
            .unwrap();
        let Frame::Binary(payload) = payload else {
            panic!("expected the binary payload next");
        };
        assert_eq!(payload.len(), 3840);
        let close = registry
            .dispatch(
                "media.microphone.close",
                ctx,
                json!({"microphone": reply["microphone"]}),
            )
            .await
            .expect("the close must succeed");
        let Reply::Json(close) = close else {
            panic!("expected a JSON close reply");
        };
        assert!(close["chunks"].as_u64().unwrap() >= 1);
        let end = tokio::time::timeout(WAIT, reader.next_frame())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(end, Frame::End));
    }

    #[tokio::test]
    async fn closing_the_session_stops_the_microphone_producer() {
        let microphone = Arc::new(VirtualMicrophone::default());
        let registry = registry_with(microphone.clone());
        let ctx = CallContext::new(session().await, permissions(Some((true, true)), true));
        registry
            .dispatch(
                "media.microphone.open",
                ctx.clone(),
                json!({"id": "virtual-0"}),
            )
            .await
            .expect("the open must succeed");
        tokio::time::timeout(WAIT, ctx.session.close())
            .await
            .expect("the session must close in time");
        tokio::time::timeout(std::time::Duration::from_secs(5), async {
            while microphone.active_streams() != 0 {
                tokio::time::sleep(std::time::Duration::from_millis(10)).await;
            }
        })
        .await
        .expect("the streams must settle in time");
    }
}
