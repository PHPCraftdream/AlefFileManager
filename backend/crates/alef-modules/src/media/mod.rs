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

use std::sync::{Arc, Mutex};

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
/// stream is gone. A canceled relay (the resource was closed) drops the frame it was mid-way
/// through and ends the stream, instead of staying suspended on the credit of a full window.
pub(crate) async fn relay<T: StreamItem + Sync>(
    writer: StreamWriter,
    stream: Arc<DeviceStream<T>>,
    mut cancel: tokio::sync::watch::Receiver<bool>,
) {
    let queue = stream.queue();
    let mut canceled = false;
    loop {
        // The cancel side wins over an empty queue too: a close between frames returns at once
        // instead of waiting for the next tick of the producer.
        let item = tokio::select! {
            biased;
            _ = relay_canceled(&mut cancel) => { canceled = true; break; }
            item = queue.pop() => match item {
                Some(item) => item,
                None => break,
            },
        };
        let send = async {
            writer.send_json(item.header()).await?;
            writer.send_binary(item.payload()).await
        };
        tokio::select! {
            biased;
            _ = relay_canceled(&mut cancel) => { canceled = true; break; }
            sent = send => { if sent.is_err() { break; } }
        }
    }
    stream.stop();
    if canceled {
        writer.end_discarding_pending();
    } else {
        writer.end();
    }
}

/// Resolves once the relay is canceled; a dropped cancel side (the resource is gone without a
/// close) ends the relay too.
async fn relay_canceled(cancel: &mut tokio::sync::watch::Receiver<bool>) {
    while !*cancel.borrow_and_update() {
        if cancel.changed().await.is_err() {
            return;
        }
    }
}

/// A device relay under the control of its resource: closing flips the cancel flag and joins the
/// task, so the close of the resource returns only after the relay ended the stream.
pub(crate) struct RelayHandle {
    cancel: tokio::sync::watch::Sender<bool>,
    task: Mutex<Option<tokio::task::JoinHandle<()>>>,
}

impl RelayHandle {
    /// Spawns the relay of an opened device stream; the resource owns the handle.
    pub(crate) fn spawn<T: StreamItem + Sync>(
        writer: StreamWriter,
        stream: Arc<DeviceStream<T>>,
    ) -> Self {
        let (cancel, canceled) = tokio::sync::watch::channel(false);
        let task = tokio::spawn(relay(writer, stream, canceled));
        Self {
            cancel,
            task: Mutex::new(Some(task)),
        }
    }

    /// Cancels the relay, stops the producer and waits for the relay's end; the relay itself ends
    /// the stream. Idempotent: the task is joined once. Stopping the producer here wakes a relay
    /// parked on an empty queue at once, so a producer parked between ticks cannot hold the
    /// cleanup.
    pub(crate) async fn shutdown<T: StreamItem + Sync>(&self, stream: &DeviceStream<T>) {
        let _ = self.cancel.send(true);
        // Ends `pop` (queue closed) and wakes the parked producer at once.
        stream.stop();
        let task = self.task.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(task) = task {
            let _ = task.await;
        }
    }
}

pub(crate) fn register(registry: &mut Registry, context: &ModuleContext) -> Result<(), AlefError> {
    camera::register(registry, context.backends.camera.clone())?;
    microphone::register(registry, context.backends.microphone.clone())?;
    audio::register(registry, context.backends.audio.clone())
}
