// SPDX-License-Identifier: MIT OR Apache-2.0
//! Verified updater package downloads.
use super::{client, document, storage, State, UpdaterContext};
use alef_core::{
    ids::ResourceId,
    registry::{command::Reply, dispatch::Registry},
    session::{resources::Resource, streams::StreamWriter},
    AlefError, ErrorCode,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use http_body_util::BodyExt;
use hyper::header::CONTENT_LENGTH;
use serde::Deserialize;
use serde_json::json;
use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex},
    time::Duration,
};
use tokio::{
    fs::File as AsyncFile,
    io::AsyncWriteExt,
    sync::{OwnedSemaphorePermit, Semaphore},
    task::JoinHandle,
};

pub(super) fn register(
    registry: &mut Registry,
    context: &Option<UpdaterContext>,
    state: Arc<State>,
    gate: Arc<Semaphore>,
) -> Result<(), AlefError> {
    let config = context.clone();
    registry
        .command::<Empty>("updater.download")?
        .waits()
        .handler(move |ctx, _args| {
            let config = config.clone();
            let state = state.clone();
            let gate = gate.clone();
            async move {
                let config = config.ok_or_else(|| {
                    AlefError::new(ErrorCode::NotAvailable, "native updater is unavailable")
                })?;
                let permit = gate.try_acquire_owned().map_err(|_| {
                    AlefError::new(ErrorCode::UpdateBusy, "updater operation already active")
                })?;
                let offered = state
                    .offered
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .clone()
                    .ok_or_else(|| {
                        AlefError::new(ErrorCode::UpdateNotAvailable, "no update is offered")
                    })?;
                let (writer, stream) = ctx.streams().open_outgoing();
                let task = tokio::spawn(run(config, offered, permit, writer));
                let resource_id = ctx
                    .resources()
                    .insert(Box::new(DownloadTask(Mutex::new(Some(task)))))?;
                Ok(Reply::Json(
                    json!({"stream": stream.0, "resource": resource_id.0}),
                ))
            }
        })?;
    registry
        .command::<CloseArgs>("updater.close")?
        .handler(|ctx, args| async move {
            let id = ResourceId(args.id);
            ctx.resources().with_as::<DownloadTask, _>(id, |_| ())?;
            ctx.resources().take(id)?.close().await;
            Ok(Reply::Json(serde_json::Value::Null))
        })?;
    Ok(())
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Empty {}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CloseArgs {
    id: u64,
}
struct DownloadTask(Mutex<Option<JoinHandle<()>>>);
impl Resource for DownloadTask {
    fn close(self: Box<Self>) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        Box::pin(async move {
            let task = self.0.lock().unwrap_or_else(|e| e.into_inner()).take();
            if let Some(task) = task {
                task.abort();
                let _ = task.await;
            }
        })
    }
}
impl Drop for DownloadTask {
    fn drop(&mut self) {
        if let Some(task) = self.0.get_mut().unwrap_or_else(|e| e.into_inner()).take() {
            task.abort();
        }
    }
}

async fn run(
    config: UpdaterContext,
    offered: document::Document,
    _permit: OwnedSemaphorePermit,
    writer: StreamWriter,
) {
    let outcome = tokio::time::timeout(Duration::from_secs(600), async {
        let (received, total) = transfer(&config, &offered, &writer, MAX_PACKAGE_BYTES).await?;
        writer
            .send_json(json!({"received":received,"total":total,"done":true}))
            .await?;
        Ok::<_, AlefError>(())
    })
    .await;
    match outcome {
        Ok(Ok(())) => writer.end(),
        Ok(Err(e)) => writer.error(e),
        Err(_) => writer.error(AlefError::new(
            ErrorCode::UpdateTimeout,
            "updater download timed out",
        )),
    }
}
pub(super) async fn transfer(
    config: &UpdaterContext,
    offered: &document::Document,
    writer: &StreamWriter,
    transfer_limit: u64,
) -> Result<(u64, u64), AlefError> {
    let storage = storage::Storage::prepare(&config.data)?;
    let expected = STANDARD
        .decode(&config.config.public_key)
        .map_err(|_| AlefError::new(ErrorCode::UpdateKeyMismatch, "invalid trusted updater key"))?;
    if expected.as_slice() != config.trusted_key {
        return Err(AlefError::new(
            ErrorCode::UpdateKeyMismatch,
            "configured updater key does not match trusted key",
        ));
    }
    let response = client::get_response(
        &url::Url::parse(&offered.package)
            .map_err(|_| AlefError::new(ErrorCode::UpdateBadDocument, "invalid package URL"))?,
    )
    .await?;
    let len = response
        .headers()
        .get(CONTENT_LENGTH)
        .and_then(|v| v.to_str().ok())
        .and_then(|s| s.parse::<u64>().ok());
    if len.is_some_and(|n| n > transfer_limit) {
        return Err(AlefError::new(
            ErrorCode::UpdateTooLarge,
            "package exceeds 512 MiB",
        ));
    }
    let mut temp = storage.temp()?;
    let mut file = AsyncFile::from_std(
        temp.as_file()
            .try_clone()
            .map_err(|e| storage::io_error("clone download file", temp.path(), e))?,
    );
    let mut body = response.into_body();
    let mut hash = ring::digest::Context::new(&ring::digest::SHA256);
    let mut received = 0u64;
    let mut next_progress = 256 * 1024u64;
    while let Some(frame) = body.frame().await {
        let frame = frame.map_err(|error| {
            client::diagnostic("download-body", &offered.package, &error);
            AlefError::new(ErrorCode::UpdateNetwork, "updater response body failed")
        })?;
        if let Some(data) = frame.data_ref() {
            received = received
                .checked_add(data.len() as u64)
                .ok_or_else(|| AlefError::new(ErrorCode::UpdateTooLarge, "package too large"))?;
            if received > transfer_limit {
                return Err(AlefError::new(
                    ErrorCode::UpdateTooLarge,
                    "package exceeds 512 MiB",
                ));
            }
            file.write_all(data)
                .await
                .map_err(|e| storage::io_error("write download", temp.path(), e))?;
            hash.update(data);
            if received >= next_progress {
                writer
                    .send_json(json!({"received":received,"total":len}))
                    .await
                    .map_err(|_| AlefError::new(ErrorCode::Closed, "download stream closed"))?;
                next_progress = (received / (256 * 1024) + 1) * (256 * 1024);
            }
        }
    }
    file.flush()
        .await
        .map_err(|e| storage::io_error("flush download", temp.path(), e))?;
    file.sync_all()
        .await
        .map_err(|e| storage::io_error("sync download", temp.path(), e))?;
    drop(file);
    if hex(hash.finish().as_ref()) != offered.sha256 {
        return Err(AlefError::new(
            ErrorCode::UpdateHashMismatch,
            "package digest mismatch",
        ));
    }
    let total = len.unwrap_or(received);
    storage::verify_and_publish(storage, temp, config, offered).await?;
    Ok((received, total))
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
pub(super) const MAX_PACKAGE_BYTES: u64 = 512 * 1024 * 1024;
