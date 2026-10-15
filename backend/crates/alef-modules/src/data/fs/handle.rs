// SPDX-License-Identifier: MIT OR Apache-2.0
//! Open files: a handle is a resource of the session (closed with it), read and written piece by
//! piece, or through streams of the transport that carry backpressure both ways, so that a file of
//! any size passes with a bounded buffer.
use std::{
    fs::{File, OpenOptions},
    future::Future,
    io::{Read, Seek, SeekFrom, Write},
    pin::Pin,
    sync::{Arc, Mutex},
};

use alef_core::{
    ids::ResourceId,
    registry::{command::Reply, context::CallContext, dispatch::Registry},
    security::permissions::{refusal, Permission, Reach},
    session::Resource,
    AlefError, ErrorCode,
};
use bytes::Bytes;
use serde::Deserialize;
use serde_json::json;
use tokio::task::JoinHandle;

use super::{
    meta::{
        dto::FileStat,
        fault::{coded, fault, invalid},
    },
    ops::{file_stat, open_for_handle},
    space::Space,
};
use crate::json;

/// The most one `fs.read` gives; a stream is for more.
const MAX_PIECE: u64 = 16 * 1024 * 1024;
/// The size of what a stream pump reads at a time.
const STREAM_PIECE: u64 = 256 * 1024;

struct Cursor {
    file: File,
    /// Where `read` and `write` without a position go: the file position of the handle.
    offset: u64,
}

/// What a running stream pump does for the handle.
enum Pump {
    Read(JoinHandle<()>),
    Write(JoinHandle<Result<(), AlefError>>),
}

struct Inner {
    cursor: Mutex<Cursor>,
    readable: bool,
    writable: bool,
    append: bool,
    pump: Mutex<Option<Pump>>,
}

impl Inner {
    fn lock(&self) -> std::sync::MutexGuard<'_, Cursor> {
        self.cursor.lock().unwrap_or_else(|e| e.into_inner())
    }

    /// Reads up to `len` bytes at `position`, or at the position of the handle which then moves on.
    fn read(&self, len: u64, position: Option<u64>) -> Result<Vec<u8>, AlefError> {
        let mut cursor = self.lock();
        let at = position.unwrap_or(cursor.offset);
        cursor.file.seek(SeekFrom::Start(at)).map_err(fault)?;
        let mut data = Vec::new();
        (&cursor.file)
            .take(len)
            .read_to_end(&mut data)
            .map_err(fault)?;
        if position.is_none() {
            cursor.offset = at + data.len() as u64;
        }
        Ok(data)
    }

    /// Writes all of `data` at `position`, or at the position of the handle, which then moves on; a
    /// file opened to append takes everything at its end.
    fn write(&self, data: &[u8], position: Option<u64>) -> Result<(), AlefError> {
        let mut cursor = self.lock();
        if self.append {
            return cursor.file.write_all(data).map_err(fault);
        }
        let at = position.unwrap_or(cursor.offset);
        cursor.file.seek(SeekFrom::Start(at)).map_err(fault)?;
        cursor.file.write_all(data).map_err(fault)?;
        if position.is_none() {
            cursor.offset = at + data.len() as u64;
        }
        Ok(())
    }

    fn stat(&self) -> Result<FileStat, AlefError> {
        let cursor = self.lock();
        Ok(file_stat(&cursor.file.metadata().map_err(fault)?))
    }

    fn truncate(&self, len: u64) -> Result<(), AlefError> {
        self.lock().file.set_len(len).map_err(fault)
    }

    fn sync(&self) -> Result<(), AlefError> {
        self.lock().file.sync_all().map_err(fault)
    }

    /// Starts the pump `make` builds, unless one is running: nothing is opened or spawned for a
    /// call that gets `BUSY`, so it cannot take a piece of the file from the stream that runs.
    fn start<T>(&self, make: impl FnOnce() -> (Pump, T)) -> Result<T, AlefError> {
        let mut slot = self.pump.lock().unwrap_or_else(|e| e.into_inner());
        let running = match slot.as_ref() {
            Some(Pump::Read(task)) => !task.is_finished(),
            Some(Pump::Write(task)) => !task.is_finished(),
            None => false,
        };
        if running {
            return Err(coded(ErrorCode::Busy));
        }
        let (pump, started) = make();
        *slot = Some(pump);
        Ok(started)
    }

    fn take_write_pump(&self) -> Option<JoinHandle<Result<(), AlefError>>> {
        let mut slot = self.pump.lock().unwrap_or_else(|e| e.into_inner());
        if matches!(slot.as_ref(), Some(Pump::Write(_))) {
            match slot.take() {
                Some(Pump::Write(task)) => Some(task),
                _ => unreachable!(),
            }
        } else {
            None
        }
    }

    fn take_pump(&self) -> Option<Pump> {
        self.pump.lock().unwrap_or_else(|e| e.into_inner()).take()
    }
}

/// The resource a handle is: closed when the document goes away.
pub(super) struct OpenFile {
    inner: Arc<Inner>,
}

impl Resource for OpenFile {
    fn close(self: Box<Self>) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        Box::pin(async move {
            match self.inner.take_pump() {
                Some(Pump::Read(task)) => task.abort(),
                Some(Pump::Write(task)) => task.abort(),
                None => {}
            }
            // The file closes when the last piece of work that holds it is done.
            let _ = tokio::task::spawn_blocking(move || drop(self)).await;
        })
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct OpenArgs {
    path: String,
    #[serde(default)]
    read: bool,
    #[serde(default)]
    write: bool,
    #[serde(default)]
    append: bool,
    #[serde(default)]
    create: bool,
    #[serde(default)]
    truncate: bool,
    #[serde(default)]
    create_new: bool,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Handle {
    handle: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadArgs {
    handle: u64,
    length: u64,
    #[serde(default)]
    position: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteArgs {
    handle: u64,
    #[serde(default)]
    position: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct TruncateArgs {
    handle: u64,
    length: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StreamArgs {
    handle: u64,
    #[serde(default)]
    position: Option<u64>,
    /// How much a read stream gives; to the end of the file when absent.
    #[serde(default)]
    length: Option<u64>,
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, AlefError> + Send + 'static,
) -> Result<T, AlefError> {
    super::blocking(work).await
}

/// The handle of the session, looked up as the kind it must be.
fn find(ctx: &CallContext, handle: u64) -> Result<Arc<Inner>, AlefError> {
    ctx.resources()
        .with_as::<OpenFile, _>(ResourceId(handle), |file| file.inner.clone())
}

fn flags(args: &OpenArgs) -> Result<(bool, bool), AlefError> {
    let writes = args.write || args.append || args.create || args.truncate || args.create_new;
    let reads = args.read || !writes;
    if args.truncate && !(args.write || args.create_new) {
        return Err(invalid("truncate needs write"));
    }
    if args.append && args.truncate {
        return Err(invalid("append and truncate exclude each other"));
    }
    if (args.create || args.create_new) && !(args.write || args.append) {
        return Err(invalid("create needs write or append"));
    }
    Ok((reads, writes))
}

pub(super) fn register(registry: &mut Registry, space: &Space) -> Result<(), AlefError> {
    let at = space.clone();
    registry
        .command::<OpenArgs>("fs.open")?
        .handler(move |ctx, args| {
            let at = at.clone();
            async move {
                let (reads, writes) = flags(&args)?;
                let mut place = None;
                if writes {
                    place = Some(
                        at.place(&ctx, Permission::FsWrite, &args.path, Reach::Through)
                            .await?,
                    );
                }
                if reads {
                    let seen = at
                        .place(&ctx, Permission::FsRead, &args.path, Reach::Through)
                        .await?;
                    // Both rights decided differently for the same path: one file cannot be real for
                    // reading and a stand-in for writing.
                    if place
                        .as_ref()
                        .is_some_and(|written| written.real != seen.real)
                    {
                        return Err(refusal(Permission::FsRead));
                    }
                    place = Some(seen);
                }
                let place = place.expect("a file is opened for reading or writing");
                let append = args.append;
                let file = blocking(move || {
                    let mut options = OpenOptions::new();
                    options
                        .read(reads)
                        .write(args.write || args.truncate || args.create_new)
                        .append(append)
                        .create(args.create && !args.create_new)
                        .create_new(args.create_new);
                    open_for_handle(&place, &options, args.truncate && !args.create_new)
                })
                .await?;
                let resource = OpenFile {
                    inner: Arc::new(Inner {
                        cursor: Mutex::new(Cursor { file, offset: 0 }),
                        readable: reads,
                        writable: writes,
                        append,
                        pump: Mutex::new(None),
                    }),
                };
                let id = ctx.resources().insert(Box::new(resource))?;
                json(&json!({ "handle": id.0 }))
            }
        })?;

    registry
        .command::<ReadArgs>("fs.read")?
        .handler(|ctx, args| async move {
            let inner = find(&ctx, args.handle)?;
            if !inner.readable {
                return Err(coded(ErrorCode::PermissionDenied));
            }
            if args.length > MAX_PIECE {
                return Err(invalid("more than 16 MiB at a time: read a stream"));
            }
            let data = blocking(move || inner.read(args.length, args.position)).await?;
            Ok(Reply::Bytes(Bytes::from(data)))
        })?;

    registry
        .command::<WriteArgs>("fs.write")?
        .handler(|ctx, args| async move {
            let inner = find(&ctx, args.handle)?;
            if !inner.writable {
                return Err(coded(ErrorCode::PermissionDenied));
            }
            let data = ctx.body().cloned().unwrap_or_default();
            let written = data.len();
            blocking(move || inner.write(&data, args.position)).await?;
            json(&json!({ "written": written }))
        })?;

    registry
        .command::<Handle>("fs.fstat")?
        .handler(|ctx, args| async move {
            let inner = find(&ctx, args.handle)?;
            json(&blocking(move || inner.stat()).await?)
        })?;

    registry
        .command::<TruncateArgs>("fs.truncate")?
        .handler(|ctx, args| async move {
            let inner = find(&ctx, args.handle)?;
            if !inner.writable {
                return Err(coded(ErrorCode::PermissionDenied));
            }
            blocking(move || inner.truncate(args.length)).await?;
            Ok(Reply::Json(serde_json::Value::Null))
        })?;

    registry
        .command::<Handle>("fs.sync")?
        .handler(|ctx, args| async move {
            let inner = find(&ctx, args.handle)?;
            blocking(move || inner.sync()).await?;
            Ok(Reply::Json(serde_json::Value::Null))
        })?;

    registry
        .command::<StreamArgs>("fs.readStream")?
        .handler(|ctx, args| async move {
            let inner = find(&ctx, args.handle)?;
            if !inner.readable {
                return Err(coded(ErrorCode::PermissionDenied));
            }
            let id = inner.start(|| {
                let (writer, id) = ctx.streams().open_outgoing();
                let mut position = args.position;
                let mut left = args.length;
                let reading = inner.clone();
                let task = tokio::spawn(async move {
                    loop {
                        let want = left.map_or(STREAM_PIECE, |left| left.min(STREAM_PIECE));
                        if want == 0 {
                            writer.end();
                            return;
                        }
                        let source = reading.clone();
                        let piece =
                            tokio::task::spawn_blocking(move || source.read(want, position))
                                .await
                                .map_err(|error| {
                                    AlefError::new(ErrorCode::Internal, error.to_string())
                                })
                                .and_then(|read| read);
                        match piece {
                            Ok(piece) if piece.is_empty() => {
                                writer.end();
                                return;
                            }
                            Ok(piece) => {
                                let length = piece.len() as u64;
                                position = position.map(|at| at + length);
                                left = left.map(|left| left - length.min(left));
                                // The page closed the stream, or the document went away: stop reading.
                                if writer.send_binary(Bytes::from(piece)).await.is_err() {
                                    return;
                                }
                            }
                            Err(error) => {
                                writer.error(error);
                                return;
                            }
                        }
                    }
                });
                (Pump::Read(task), id)
            })?;
            json(&json!({ "stream": id.0 }))
        })?;

    registry
        .command::<StreamArgs>("fs.writeStream")?
        .handler(|ctx, args| async move {
            let inner = find(&ctx, args.handle)?;
            if !inner.writable {
                return Err(coded(ErrorCode::PermissionDenied));
            }
            let id = inner.start(|| {
                let (mut reader, id) = ctx.streams().open_incoming_reader();
                let mut position = args.position;
                let writing = inner.clone();
                let task = tokio::spawn(async move {
                    while let Some(piece) = reader.recv().await {
                        let piece = piece?;
                        let length = piece.len() as u64;
                        let target = writing.clone();
                        let at = position;
                        blocking(move || target.write(&piece, at)).await?;
                        position = position.map(|at| at + length);
                    }
                    Ok(())
                });
                (Pump::Write(task), id)
            })?;
            json(&json!({ "stream": id.0 }))
        })?;

    // Waits until what was written through the stream has reached the file; the error of a write
    // that failed on the way is the answer.
    registry
        .command::<Handle>("fs.settle")?
        .handler(|ctx, args| async move {
            let inner = find(&ctx, args.handle)?;
            if let Some(task) = inner.take_write_pump() {
                task.await
                    .map_err(|error| AlefError::new(ErrorCode::Internal, error.to_string()))??;
            }
            Ok(Reply::Json(serde_json::Value::Null))
        })?;

    registry
        .command::<Handle>("fs.close")?
        .handler(|ctx, args| async move {
            ctx.resources()
                .with_as::<OpenFile, _>(ResourceId(args.handle), |_| ())?;
            ctx.resources().take(ResourceId(args.handle))?.close().await;
            Ok(Reply::Json(serde_json::Value::Null))
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    use alef_core::{
        ids::StreamId,
        protocol::{call::Limits, frame::Frame},
        security::{
            manifest::Permissions,
            permissions::{PathVars, PermissionSet},
        },
        session::SessionManager,
    };

    #[tokio::test]
    async fn settle_preserves_a_backpressured_read_and_its_busy_slot() {
        tokio::time::timeout(Duration::from_secs(10), async {
            let directory = match option_env!("CARGO_TARGET_TMPDIR") {
                Some(target) => tempfile::tempdir_in(target),
                None => tempfile::tempdir(),
            }
            .unwrap();
            let path = directory.path().to_path_buf();
            let policy: Permissions = serde_json::from_value(json!({
                "fs": {"read": [], "write": []}, "cli": {"exec": []},
                "net": {"http": [], "socket": []}, "shell": {"openExternal": []},
                "clipboard": {"read": false}, "shortcut": {"global": false},
                "secrets": false, "app": {"env": []}
            }))
            .unwrap();
            let permissions = Arc::new(
                PermissionSet::from_manifest(
                    &policy,
                    &PathVars {
                        app_data: path.clone(),
                        app_config: path.clone(),
                        app_cache: path.clone(),
                        home: path.clone(),
                        documents: path.clone(),
                        downloads: path.clone(),
                        desktop: path.clone(),
                        temp: path.clone(),
                        app: Some(path.clone()),
                    },
                )
                .unwrap(),
            );
            let limits = Limits {
                stream_window: STREAM_PIECE as usize,
                chunk_size: STREAM_PIECE as usize,
                ..Limits::default()
            };
            let manager = SessionManager::new(Arc::new(|| "test-token".to_owned()), limits);
            let session = manager.begin_document(1).await;
            let ctx = CallContext::new(session.clone(), permissions);
            let mut registry = Registry::default();
            register(&mut registry, &Space::new(path.join("shadow"))).unwrap();
            let file = path.join("read.bin");
            let data: Vec<u8> = (0..3 * STREAM_PIECE as usize + 123)
                .map(|i| (i % 251) as u8)
                .collect();
            std::fs::write(&file, &data).unwrap();
            session.grants().grant_read(&file).unwrap();
            let Reply::Json(opened) = registry
                .dispatch("fs.open", ctx.clone(), json!({"path": file}))
                .await
                .unwrap()
            else {
                panic!("expected handle");
            };
            let handle = opened["handle"].as_u64().unwrap();
            let args = json!({"handle": handle});
            let Reply::Json(started) = registry
                .dispatch("fs.readStream", ctx.clone(), args.clone())
                .await
                .unwrap()
            else {
                panic!("expected stream");
            };
            let id = StreamId(started["stream"].as_u64().unwrap());
            let mut reader = session.streams().reader(id).unwrap();
            let Some(Frame::Binary(first)) = reader.next_frame().await else {
                panic!("expected first binary frame");
            };
            assert_eq!(first.len(), limits.stream_window);
            assert_eq!(session.streams().outstanding(id), Some(limits.stream_window));
            let inner = find(&ctx, handle).unwrap();
            assert!(matches!(
                inner.pump.lock().unwrap().as_ref(),
                Some(Pump::Read(task)) if !task.is_finished()
            ));

            for _ in 0..2 {
                let settled = registry
                    .dispatch("fs.settle", ctx.clone(), args.clone())
                    .await
                    .unwrap();
                assert!(matches!(settled, Reply::Json(value) if value.is_null()));
                assert!(
                    matches!(inner.pump.lock().unwrap().as_ref(), Some(Pump::Read(task)) if !task.is_finished()),
                    "settle must leave the active read pump reserved"
                );
                let error = registry
                    .dispatch("fs.readStream", ctx.clone(), args.clone())
                    .await
                    .unwrap_err();
                assert_eq!(error.code, ErrorCode::Busy);
            }

            let mut received = first.to_vec();
            session.streams().ack(id, first.len()).unwrap();
            loop {
                match reader.next_frame().await {
                    Some(Frame::Binary(piece)) => {
                        session.streams().ack(id, piece.len()).unwrap();
                        received.extend_from_slice(&piece);
                    }
                    Some(Frame::End) => break,
                    other => panic!("expected binary frame or End, got {other:?}"),
                }
            }
            assert_eq!(received.len(), data.len());
            assert!(received == data, "settle must not truncate or change the read");
            registry.dispatch("fs.close", ctx, args).await.unwrap();
            drop(inner);
            manager.close_window(1).await;
        })
        .await
        .expect("settle/read regression stalled");
    }
}
