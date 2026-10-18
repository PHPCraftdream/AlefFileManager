// SPDX-License-Identifier: MIT OR Apache-2.0
//! The `media` module: cameras, microphones and played audio behind the [`CameraBackend`],
//! [`MicrophoneBackend`] and [`AudioBackend`] traits. The virtual backends serve the end-to-end
//! runs and the tests, the Missing ones answer `NotAvailable`. Frames and chunks travel as JSON
//! header and binary payload pairs on a credit-based stream: an admitted blocking job paces the
//! producer, a bounded drop-oldest queue keeps the newest frames, and the relay forwards
//! them off the bus.
pub mod audio;
pub mod backend;
pub mod camera;
pub mod microphone;
pub mod queue;
pub mod virtuals;

pub use backend::{
    AudioBackend, AudioSink, CameraBackend, CameraInfo, DeviceStream, MicrophoneBackend,
    MicrophoneInfo, MissingAudio, MissingCamera, MissingMicrophone,
};
pub use queue::{CameraFrame, MediaQueue, PcmChunk, StreamItem};
pub use virtuals::{VirtualAudio, VirtualCamera, VirtualMicrophone};

use std::sync::Arc;

use alef_core::session::streams::StreamWriter;
use alef_core::{
    registry::{context::CallContext, dispatch::Registry},
    security::consent::Decision,
    AlefError, ErrorCode,
};

use crate::ModuleContext;

pub(crate) fn invalid(message: String) -> AlefError {
    AlefError::new(ErrorCode::InvalidArgument, message)
}

/// The blocking budget of the media module: device open/close, producer jobs and sample
/// checksumming. A local budget, not a global or per-session framework limit.
pub(crate) fn executor() -> &'static alef_core::session::BlockingExecutor {
    static EXECUTOR: std::sync::OnceLock<alef_core::session::BlockingExecutor> =
        std::sync::OnceLock::new();
    EXECUTOR.get_or_init(|| {
        alef_core::session::BlockingExecutor::new(
            std::num::NonZeroUsize::new(8).expect("media budget"),
        )
    })
}

/// A right the user substituted is answered without touching a device.
pub(crate) fn substituted(ctx: &CallContext, command: &str) -> Result<(), AlefError> {
    (ctx.decision() != Decision::Substitute)
        .then_some(())
        .ok_or_else(|| {
            AlefError::new(
                ErrorCode::NotAvailable,
                format!("{command}: substituted, no device is touched"),
            )
        })
}

/// A device stream relay: headers and payloads go out in order; the producer is stopped when the
/// stream is gone.
pub(crate) async fn relay<T: StreamItem>(writer: StreamWriter, stream: Arc<DeviceStream<T>>) {
    let queue = stream.queue();
    while let Some(item) = queue.pop().await {
        if writer.send_json(item.header()).await.is_err()
            || writer.send_binary(item.payload()).await.is_err()
        {
            break;
        }
    }
    stream.stop();
    writer.end();
}

pub(crate) fn register(registry: &mut Registry, context: &ModuleContext) -> Result<(), AlefError> {
    camera::register(registry, context.backends.camera.clone())?;
    microphone::register(registry, context.backends.microphone.clone())?;
    audio::register(registry, context.backends.audio.clone())
}
