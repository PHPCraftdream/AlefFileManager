// SPDX-License-Identifier: MIT OR Apache-2.0
//! Snapshot-based volume change stream. Native handles are opened, waited, and dropped on blocking workers.
use super::super::meta::dto::Volume;
use alef_core::{
    registry::context::CallContext,
    session::{Resource, Session},
    AlefError, ErrorCode,
};
#[cfg(test)]
use std::time::Instant;
use std::{
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    time::Duration,
};
use tokio::sync::{mpsc, oneshot};

const POLL: Duration = Duration::from_secs(1);
struct Lease(Arc<AtomicBool>);
impl Drop for Lease {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Release);
    }
}
impl Resource for Lease {
    fn close(self: Box<Self>) -> std::pin::Pin<Box<dyn std::future::Future<Output = ()> + Send>> {
        self.0.store(true, Ordering::Release);
        Box::pin(async {})
    }
}
struct RemoveLease {
    session: Arc<Session>,
    id: alef_core::ids::ResourceId,
}
impl Drop for RemoveLease {
    fn drop(&mut self) {
        let _ = self.session.resources().remove(self.id);
    }
}
struct StopOnDrop(Option<Arc<AtomicBool>>);
impl StopOnDrop {
    fn disarm(&mut self) {
        self.0.take();
    }
}
impl Drop for StopOnDrop {
    fn drop(&mut self) {
        if let Some(stop) = &self.0 {
            stop.store(true, Ordering::Release);
        }
    }
}

pub(super) async fn start(
    ctx: &CallContext,
) -> Result<alef_core::registry::command::Reply, AlefError> {
    let stop = Arc::new(AtomicBool::new(false));
    let id = ctx.resources().insert(Box::new(Lease(stop.clone())))?;
    let lease = RemoveLease {
        session: ctx.session.clone(),
        id,
    };
    let (snapshots, received) = mpsc::channel::<Result<Vec<Volume>, AlefError>>(1);
    let (setup_tx, setup_rx) = oneshot::channel();
    let worker_stop = stop.clone();
    // This worker exclusively owns the native Source, including its destruction.
    let worker = tokio::task::spawn_blocking(move || {
        let mut setup = Some(setup_tx);
        let init = (|| {
            let mut source = Source::open()?;
            let mut snapshot = super::list()?;
            if let Some(tx) = setup.take() {
                let _ = tx.send(Ok(snapshot.clone()));
            }
            while !worker_stop.load(Ordering::Acquire) {
                if !source.wait(&worker_stop)? || worker_stop.load(Ordering::Acquire) {
                    continue;
                }
                let next = super::list()?;
                if next != snapshot {
                    snapshot = next.clone();
                    if snapshots.blocking_send(Ok(next)).is_err() {
                        break;
                    }
                }
            }
            Ok::<(), AlefError>(())
        })();
        if let Err(error) = init {
            if let Some(tx) = setup.take() {
                let _ = tx.send(Err(error));
            } else {
                let _ = snapshots.blocking_send(Err(error));
            }
        }
    });
    let mut setup_cancel = StopOnDrop(Some(stop.clone()));
    let initial = match setup_rx.await {
        Ok(Ok(initial)) => initial,
        Ok(Err(error)) => {
            stop.store(true, Ordering::Release);
            let _ = worker.await;
            return Err(error);
        }
        Err(_) => {
            stop.store(true, Ordering::Release);
            let _ = worker.await;
            return Err(unavailable("volume watcher setup failed"));
        }
    };
    setup_cancel.disarm();
    let (writer, stream_id) = ctx.streams().open_outgoing();
    let pump_stop = stop.clone();
    // Intentionally detached: the per-session lease owns cancellation; stream close stops the worker.
    tokio::spawn(pump(writer, received, initial, worker, pump_stop, lease));
    Ok(alef_core::registry::command::Reply::Json(
        serde_json::json!({ "stream": stream_id.0 }),
    ))
}

async fn pump(
    writer: alef_core::session::StreamWriter,
    mut received: mpsc::Receiver<Result<Vec<Volume>, AlefError>>,
    initial: Vec<Volume>,
    worker: tokio::task::JoinHandle<()>,
    stop: Arc<AtomicBool>,
    lease: RemoveLease,
) {
    let _lease = lease;
    let _stop_guard = StopOnDrop(Some(stop.clone()));
    let mut before = initial;
    let mut terminal_error = None;
    loop {
        tokio::select! {
            biased;
            _ = writer.closed() => break,
            message = received.recv() => match message {
                Some(Ok(after)) => {
                    let mut closed = false;
                    for event in super::diff(&before, &after) {
                        tokio::select! {
                            _ = writer.closed() => { closed = true; break; }
                            sent = writer.send_json(serde_json::to_value(event).unwrap_or_default()) => if sent.is_err() { closed = true; break; }
                        }
                    }
                    if closed { break; }
                    before = after;
                }
                Some(Err(error)) => { terminal_error = Some(error); break; }
                None => break,
            }
        }
    }
    // Drop the receiver before joining: otherwise a full bounded queue could strand blocking_send.
    drop(received);
    stop.store(true, Ordering::Release);
    let joined = worker.await;
    match terminal_error {
        Some(error) => writer.error(error),
        None if joined.is_err() => writer.error(unavailable("volume watcher worker failed")),
        None => writer.end(),
    }
}

struct Source {
    #[cfg(target_os = "linux")]
    mountinfo: std::fs::File,
    #[cfg(target_os = "macos")]
    _watcher: notify::RecommendedWatcher,
    #[cfg(target_os = "macos")]
    events: std::sync::mpsc::Receiver<()>,
}
impl Source {
    fn open() -> Result<Self, AlefError> {
        #[cfg(target_os = "linux")]
        {
            Ok(Self {
                mountinfo: std::fs::File::open("/proc/self/mountinfo")
                    .map_err(|_| unavailable("mountinfo unavailable"))?,
            })
        }
        #[cfg(target_os = "macos")]
        {
            use notify::Watcher;
            let (tx, events) = std::sync::mpsc::sync_channel(1);
            let mut watcher = notify::RecommendedWatcher::new(
                move |_| {
                    let _ = tx.try_send(());
                },
                notify::Config::default(),
            )
            .map_err(|_| unavailable("volume watcher unavailable"))?;
            watcher
                .watch(
                    std::path::Path::new("/Volumes"),
                    notify::RecursiveMode::NonRecursive,
                )
                .map_err(|_| unavailable("volume watch unavailable"))?;
            Ok(Self {
                _watcher: watcher,
                events,
            })
        }
        #[cfg(windows)]
        {
            Ok(Self {})
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
        {
            Ok(Self {})
        }
    }
    fn wait(&mut self, stop: &AtomicBool) -> Result<bool, AlefError> {
        if stop.load(Ordering::Acquire) {
            return Ok(false);
        }
        #[cfg(target_os = "linux")]
        {
            use std::{
                io::{Read, Seek, SeekFrom},
                os::fd::AsRawFd,
            };
            let mut descriptor = libc::pollfd {
                fd: self.mountinfo.as_raw_fd(),
                events: libc::POLLPRI | libc::POLLERR,
                revents: 0,
            };
            // SAFETY: descriptor points to one initialized pollfd for the duration of poll.
            let result = unsafe { libc::poll(&mut descriptor, 1, POLL.as_millis() as i32) };
            if result == 0 {
                return Ok(false);
            }
            if result < 0 {
                let error = std::io::Error::last_os_error();
                if error.kind() == std::io::ErrorKind::Interrupted {
                    return Ok(false);
                }
                return Err(super::super::meta::fault::fault(error));
            }
            if descriptor.revents & libc::POLLNVAL != 0 {
                return Err(unavailable("mountinfo watch failed"));
            }
            self.mountinfo
                .seek(SeekFrom::Start(0))
                .map_err(super::super::meta::fault::fault)?;
            let mut acknowledge = Vec::new();
            self.mountinfo
                .read_to_end(&mut acknowledge)
                .map_err(super::super::meta::fault::fault)?;
            return Ok(true);
        }
        #[cfg(target_os = "macos")]
        {
            match self.events.recv_timeout(POLL) {
                Ok(()) => Ok(true),
                Err(std::sync::mpsc::RecvTimeoutError::Timeout) => Ok(false),
                Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                    Err(unavailable("volume watcher source stopped"))
                }
            }
        }
        #[cfg(windows)]
        {
            std::thread::sleep(POLL);
            return Ok(!stop.load(Ordering::Acquire));
        }
        #[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
        {
            std::thread::sleep(POLL);
            Ok(false)
        }
    }
}
fn unavailable(message: &str) -> AlefError {
    AlefError::new(ErrorCode::NotAvailable, message)
}

#[cfg(test)]
mod tests {
    use super::*;
    use alef_core::{protocol::call::Limits, session::SessionManager};

    async fn session(resource_limit: usize) -> Arc<Session> {
        let mut limits = Limits::default();
        limits.max_resources_per_session = resource_limit;
        SessionManager::new(Arc::new(|| "test-token".into()), limits)
            .begin_document(1)
            .await
    }

    #[tokio::test]
    async fn idle_worker_stops_and_lease_is_removed_when_stream_closes() {
        let session = session(1).await;
        let stop = Arc::new(AtomicBool::new(false));
        let id = session
            .resources()
            .insert(Box::new(Lease(stop.clone())))
            .expect("resource admitted");
        let hub = session.streams();
        let (writer, stream_id) = hub.open_outgoing();
        let (tx, received) = mpsc::channel::<Result<Vec<Volume>, AlefError>>(1);
        let (done_tx, done_rx) = oneshot::channel();
        let worker_stop = stop.clone();
        let worker = tokio::task::spawn_blocking(move || {
            let _tx = tx;
            let deadline = Instant::now() + Duration::from_secs(5);
            while !worker_stop.load(Ordering::Acquire) && Instant::now() < deadline {
                std::thread::sleep(Duration::from_millis(1));
            }
            let _ = done_tx.send(worker_stop.load(Ordering::Acquire));
        });
        let lease = RemoveLease {
            session: session.clone(),
            id,
        };
        hub.close(stream_id).expect("close stream");
        tokio::time::timeout(
            Duration::from_secs(10),
            pump(writer, received, Vec::new(), worker, stop, lease),
        )
        .await
        .expect("pump completes");
        assert!(tokio::time::timeout(Duration::from_secs(10), done_rx)
            .await
            .expect("worker exits")
            .expect("worker reports stop"));
        assert!(session.resources().is_empty());
    }

    #[tokio::test]
    async fn full_queue_is_dropped_before_worker_join() {
        let session = session(1).await;
        let stop = Arc::new(AtomicBool::new(false));
        let id = session
            .resources()
            .insert(Box::new(Lease(stop.clone())))
            .expect("resource admitted");
        let hub = session.streams();
        let (writer, stream_id) = hub.open_outgoing();
        let (tx, received) = mpsc::channel::<Result<Vec<Volume>, AlefError>>(1);
        tx.try_send(Ok(Vec::new())).expect("prefill event queue");
        let (done_tx, done_rx) = oneshot::channel();
        let worker = tokio::task::spawn_blocking(move || {
            let sent = tx.blocking_send(Ok(Vec::new()));
            let _ = done_tx.send(sent.is_err());
        });
        let lease = RemoveLease {
            session: session.clone(),
            id,
        };
        hub.close(stream_id).expect("close stream");
        tokio::time::timeout(
            Duration::from_secs(10),
            pump(writer, received, Vec::new(), worker, stop, lease),
        )
        .await
        .expect("pump drops full receiver and joins worker");
        assert!(tokio::time::timeout(Duration::from_secs(10), done_rx)
            .await
            .expect("worker exits")
            .expect("worker reports send failure"));
        assert!(session.resources().is_empty());
    }
}
