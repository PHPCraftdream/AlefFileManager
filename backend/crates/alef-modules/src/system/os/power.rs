// SPDX-License-Identifier: MIT OR Apache-2.0
//! Power-state notification backends.
use std::sync::Arc;

use alef_core::session::BlockingExecutor;
use alef_core::{AlefError, ErrorCode};
use tokio::sync::OnceCell;

/// A change in the machine's power state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PowerEvent {
    /// The machine is suspending.
    Suspend,
    /// The machine has resumed.
    Resume,
}

impl PowerEvent {
    /// The event name used by the `os.suspend` / `os.resume` events.
    pub fn name(self) -> &'static str {
        match self {
            Self::Suspend => "os.suspend",
            Self::Resume => "os.resume",
        }
    }
}

/// Callback invoked when a power event occurs.
pub type PowerSink = Arc<dyn Fn(PowerEvent) + Send + Sync>;

/// Registration with a power backend.
pub trait PowerBackend: std::fmt::Debug + Send + Sync {
    /// Starts watching power events.
    fn watch(&self, sink: PowerSink) -> Result<PowerWatch, AlefError>;

    /// Delivers an event to this backend's registered watchers, if supported.
    fn simulate(&self, _event: PowerEvent) -> bool {
        false
    }
}

/// Owns a watcher registration and unregisters it when dropped.
pub struct PowerWatch {
    /// `Mutex<T: Send>` is `Sync`, so `PowerWatch` can live in a `OnceCell` shared across
    /// threads; the lock is fine because the cleanup runs exactly once.
    cleanup: std::sync::Mutex<Option<Box<dyn FnOnce() + Send + 'static>>>,
}

impl std::fmt::Debug for PowerWatch {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.debug_struct("PowerWatch").finish_non_exhaustive()
    }
}

impl PowerWatch {
    /// Constructs a guard for a backend registration.
    pub fn new(cleanup: impl FnOnce() + Send + 'static) -> Self {
        Self {
            cleanup: std::sync::Mutex::new(Some(Box::new(cleanup))),
        }
    }
}

impl Drop for PowerWatch {
    fn drop(&mut self) {
        if let Ok(mut slot) = self.cleanup.lock() {
            if let Some(cleanup) = slot.take() {
                cleanup();
            }
        }
    }
}

/// In-memory backend intended for tests and simulations.
#[derive(Default)]
pub struct PretendPower {
    registrations: Arc<std::sync::Mutex<std::collections::HashMap<u64, PowerSink>>>,
    next_id: std::sync::atomic::AtomicU64,
}

impl std::fmt::Debug for PretendPower {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("PretendPower")
            .finish_non_exhaustive()
    }
}

impl PowerBackend for PretendPower {
    fn watch(&self, sink: PowerSink) -> Result<PowerWatch, AlefError> {
        let id = self
            .next_id
            .fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        self.registrations
            .lock()
            .expect("power registry lock poisoned")
            .insert(id, sink);
        let registrations = Arc::clone(&self.registrations);
        Ok(PowerWatch::new(move || {
            registrations
                .lock()
                .expect("power registry lock poisoned")
                .remove(&id);
        }))
    }

    fn simulate(&self, event: PowerEvent) -> bool {
        let sinks = self
            .registrations
            .lock()
            .expect("power registry lock poisoned")
            .values()
            .cloned()
            .collect::<Vec<_>>();
        for sink in sinks {
            sink(event);
        }
        true
    }
}

/// Backend used when power notifications are unavailable.
#[derive(Debug, Default)]
pub struct MissingPower;

impl PowerBackend for MissingPower {
    fn watch(&self, _sink: PowerSink) -> Result<PowerWatch, AlefError> {
        Err(AlefError::new(
            ErrorCode::NotAvailable,
            "no power notifications here",
        ))
    }
}

/// Process-wide registration of the `os.watchPower` watcher.
static WATCHER: OnceCell<PowerWatch> = OnceCell::const_new();

/// Starts the process-wide power watcher exactly once.
///
/// The first call runs `power.watch(sink)` on the blocking executor and the resulting
/// [`PowerWatch`] stays in the process-wide cell for the rest of the process; every later call
/// resolves immediately without touching the backend again, and concurrent callers wait for the
/// first registration to finish and share its result. A `Busy` error from a full executor is
/// mapped to nothing: it is already the retryable `ErrorCode::Busy` and surfaces as such.
///
/// Cancel-safe: an aborted call drops its init future, nothing is stored, and a later call
/// simply retries the registration.
pub(crate) async fn ensure_watching(
    executor: BlockingExecutor,
    power: Arc<dyn PowerBackend>,
    sink: PowerSink,
) -> Result<(), AlefError> {
    ensure_watching_in(&WATCHER, executor, power, sink).await
}

/// The body of [`ensure_watching`], parameterized over the cell so tests can use fresh state
/// instead of the process-wide one.
pub(super) async fn ensure_watching_in(
    cell: &OnceCell<PowerWatch>,
    executor: BlockingExecutor,
    power: Arc<dyn PowerBackend>,
    sink: PowerSink,
) -> Result<(), AlefError> {
    cell.get_or_try_init(|| async {
        let job = executor.submit(move || power.watch(sink))?;
        job.result().await?
    })
    .await
    .map(|_| ())
}

/// Parses the `event` argument of `e2e.simulatePower`.
// Only the e2e command calls this in production builds; tests use it directly.
#[cfg_attr(not(any(test, feature = "e2e")), allow(dead_code))]
pub(crate) fn parse_power_event(event: &str) -> Result<PowerEvent, AlefError> {
    match event {
        "suspend" => Ok(PowerEvent::Suspend),
        "resume" => Ok(PowerEvent::Resume),
        other => Err(AlefError::new(
            ErrorCode::InvalidArgument,
            format!("unknown power event {other:?}; expected 'suspend' or 'resume'"),
        )),
    }
}
