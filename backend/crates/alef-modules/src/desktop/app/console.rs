// SPDX-License-Identifier: MIT OR Apache-2.0
//! `app.stdin`, `app.stdout` and `app.stderr`: the standard streams of a console utility (`console: true`
//! in the manifest). What arrives on stdin goes to the page as a stream with credit, and what the page writes
//! to a stream of stdout or stderr goes out whole and in order, flushed piece by piece. An application that
//! is no console utility has none of them (`NOT_AVAILABLE`).
use std::{
    fmt,
    sync::{Arc, Mutex},
    time::Duration,
};

use alef_core::{
    registry::dispatch::Registry, session::streams::IncomingReader, AlefError, ErrorCode,
};
use bytes::Bytes;
use serde_json::json;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt},
    sync::Mutex as AsyncMutex,
    task::JoinHandle,
};

use crate::{json, ModuleContext};

type Input = Box<dyn AsyncRead + Send + Unpin>;
type Output = Box<dyn AsyncWrite + Send + Unpin>;

/// The size of a piece of stdin.
const PIECE: usize = 64 * 1024;
/// How long the end of the process waits for what the page wrote to leave.
const DRAIN: Duration = Duration::from_secs(5);

struct Inner {
    stdin: Mutex<Option<Input>>,
    stdout: Arc<AsyncMutex<Output>>,
    stderr: Arc<AsyncMutex<Output>>,
    /// The tasks that write what the page sent; the end of the process waits for them.
    writing: Mutex<Vec<JoinHandle<()>>>,
    reading: Mutex<Option<JoinHandle<()>>>,
}

/// The standard streams a console utility has: the ones of the process, or others for a test.
#[derive(Clone)]
pub struct Console(Arc<Inner>);

impl fmt::Debug for Console {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Console").finish_non_exhaustive()
    }
}

impl Console {
    /// The stdin, stdout and stderr of this process.
    pub fn process() -> Self {
        Self::with(tokio::io::stdin(), tokio::io::stdout(), tokio::io::stderr())
    }

    /// Streams of the caller's choice (a test gives pipes in memory).
    pub fn with(
        stdin: impl AsyncRead + Send + Unpin + 'static,
        stdout: impl AsyncWrite + Send + Unpin + 'static,
        stderr: impl AsyncWrite + Send + Unpin + 'static,
    ) -> Self {
        Self(Arc::new(Inner {
            stdin: Mutex::new(Some(Box::new(stdin))),
            stdout: Arc::new(AsyncMutex::new(Box::new(stdout))),
            stderr: Arc::new(AsyncMutex::new(Box::new(stderr))),
            writing: Mutex::new(Vec::new()),
            reading: Mutex::new(None),
        }))
    }

    /// Waits (a few seconds at most) until what the page wrote has gone out, and stops reading stdin. The end
    /// of the process calls it after the documents are gone, when no more can come.
    pub async fn drain(&self) {
        if let Some(reading) = self
            .0
            .reading
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .take()
        {
            reading.abort();
        }
        let tasks: Vec<_> = self
            .0
            .writing
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .drain(..)
            .collect();
        #[cfg(feature = "e2e")]
        let task_count = tasks.len();
        #[cfg(feature = "e2e")]
        let started = std::time::Instant::now();
        #[cfg(feature = "e2e")]
        let timed_out = tokio::time::timeout(DRAIN, async {
            for task in tasks {
                let _ = task.await;
            }
        })
        .await
        .is_err();
        #[cfg(not(feature = "e2e"))]
        let _ = tokio::time::timeout(DRAIN, async {
            for task in tasks {
                let _ = task.await;
            }
        })
        .await;
        #[cfg(feature = "e2e")]
        let mut flush_ok = true;
        for stream in [&self.0.stdout, &self.0.stderr] {
            let flushed = stream.lock().await.flush().await;
            #[cfg(feature = "e2e")]
            if flushed.is_err() {
                flush_ok = false;
            }
            #[cfg(not(feature = "e2e"))]
            let _ = flushed;
        }
        #[cfg(feature = "e2e")]
        eprintln!(
            "ALEF_E2E console-drain tasks={} timed_out={} elapsed_ms={} flush={}",
            task_count,
            timed_out,
            started.elapsed().as_millis(),
            if flush_ok { "ok" } else { "err" },
        );
    }
}

/// Hands stdin to the page piece by piece, and ends the stream when stdin ends.
async fn read_pump(mut input: Input, writer: alef_core::session::streams::StreamWriter) {
    let mut buffer = vec![0_u8; PIECE];
    loop {
        match input.read(&mut buffer).await {
            Ok(0) => {
                writer.end();
                return;
            }
            Ok(count) => {
                if writer
                    .send_binary(Bytes::copy_from_slice(&buffer[..count]))
                    .await
                    .is_err()
                {
                    return;
                }
            }
            Err(error) => {
                writer.error(AlefError::from(error));
                return;
            }
        }
    }
}

/// Writes what the page sends to one of the output streams, flushing each piece. When the output is gone
/// (a pipe the reader closed) the stream of the page is closed, and its next write fails.
async fn write_pump(mut reader: IncomingReader, output: Arc<AsyncMutex<Output>>) {
    #[cfg(feature = "e2e")]
    let mut received = 0_u64;
    #[cfg(feature = "e2e")]
    let mut written = 0_u64;
    loop {
        match reader.recv().await {
            Some(Ok(bytes)) => {
                #[cfg(feature = "e2e")]
                {
                    received += bytes.len() as u64;
                }
                let mut output = output.lock().await;
                let wrote = output.write_all(&bytes).await;
                if wrote.is_err() {
                    #[cfg(feature = "e2e")]
                    eprintln!(
                        "ALEF_E2E console-write-pump id={} received={} written={} end=None \
                         error={}",
                        reader.id().0,
                        received,
                        written,
                        wrote.unwrap_err(),
                    );
                    return;
                }
                let flushed = output.flush().await;
                if flushed.is_err() {
                    #[cfg(feature = "e2e")]
                    eprintln!(
                        "ALEF_E2E console-write-pump id={} received={} written={} end=None \
                         error={}",
                        reader.id().0,
                        received,
                        written,
                        flushed.unwrap_err(),
                    );
                    return;
                }
                #[cfg(feature = "e2e")]
                {
                    written += bytes.len() as u64;
                }
            }
            Some(Err(_)) => {
                #[cfg(feature = "e2e")]
                eprintln!(
                    "ALEF_E2E console-write-pump id={} received={} written={} end=Error error=-",
                    reader.id().0,
                    received,
                    written,
                );
                return;
            }
            None => {
                #[cfg(feature = "e2e")]
                eprintln!(
                    "ALEF_E2E console-write-pump id={} received={} written={} end=End error=-",
                    reader.id().0,
                    received,
                    written,
                );
                return;
            }
        }
    }
}

pub(crate) fn register(registry: &mut Registry, context: &ModuleContext) -> Result<(), AlefError> {
    let unavailable = || {
        AlefError::new(
            ErrorCode::NotAvailable,
            "Standard streams require cli mode or legacy console: true",
        )
    };

    let console = context.console.clone();
    let launch = context.launch.clone();
    registry
        .command::<()>("app.stdin")?
        .handler(move |ctx, ()| {
            let console = console.clone();
            let launch = launch.clone();
            async move {
                if !launch.stdio_allowed() {
                    return Err(unavailable());
                }
                let console = console.ok_or_else(unavailable)?;
                let input = console
                    .0
                    .stdin
                    .lock()
                    .unwrap_or_else(|e| e.into_inner())
                    .take()
                    .ok_or_else(|| AlefError::new(ErrorCode::Busy, "stdin was taken already"))?;
                let (writer, stream) = ctx.streams().open_outgoing();
                *console.0.reading.lock().unwrap_or_else(|e| e.into_inner()) =
                    Some(tokio::spawn(read_pump(input, writer)));
                json(&json!({ "stream": stream.0 }))
            }
        })?;

    for (name, standard_error) in [("app.stdout", false), ("app.stderr", true)] {
        let console = context.console.clone();
        let launch = context.launch.clone();
        registry.command::<()>(name)?.handler(move |ctx, ()| {
            let console = console.clone();
            let launch = launch.clone();
            async move {
                if !launch.stdio_allowed() {
                    return Err(unavailable());
                }
                let console = console.ok_or_else(unavailable)?;
                let output = if standard_error {
                    console.0.stderr.clone()
                } else {
                    console.0.stdout.clone()
                };
                let (reader, stream) = ctx.streams().open_incoming_reader();
                let mut writing = console.0.writing.lock().unwrap_or_else(|e| e.into_inner());
                writing.retain(|task| !task.is_finished());
                writing.push(tokio::spawn(write_pump(reader, output)));
                json(&json!({ "stream": stream.0 }))
            }
        })?;
    }
    Ok(())
}
