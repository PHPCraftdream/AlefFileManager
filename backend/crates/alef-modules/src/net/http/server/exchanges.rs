// SPDX-License-Identifier: MIT OR Apache-2.0
//! Live-only routing. Lock order is routes -> table; no lock crosses an await.
//! Routes retain neither sessions nor servers. Slots last until the handler drops its lease.
use super::Exchange;
use alef_core::{session::session::Session, AlefError, ErrorCode};
use std::{
    collections::HashMap,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, LazyLock, Mutex, Weak,
    },
};
use tokio::sync::{OwnedSemaphorePermit, Semaphore};

static NEXT: AtomicU64 = AtomicU64::new(1 << 32);
static ROUTES: LazyLock<Mutex<HashMap<u64, Weak<Table>>>> =
    LazyLock::new(|| Mutex::new(HashMap::new()));
const MAX_ID: u64 = (1 << 53) - 1;

#[derive(Default)]
struct State {
    closed: bool,
    entries: HashMap<u64, Exchange>,
}
pub(super) struct Table {
    owner: Weak<Session>,
    slots: Arc<Semaphore>,
    state: Mutex<State>,
}
pub(super) struct Lease {
    pub(super) id: u64,
    table: Arc<Table>,
    _slot: OwnedSemaphorePermit,
}
pub(super) struct Closing(pub(super) Arc<Table>);
fn missing() -> AlefError {
    AlefError::new(ErrorCode::NotFound, "request not found")
}
impl Table {
    #[cfg(test)]
    pub(super) fn routed(id: u64) -> bool {
        ROUTES.lock().unwrap().contains_key(&id)
    }
    #[cfg(test)]
    pub(super) fn contains(&self, id: u64) -> bool {
        self.state.lock().unwrap().entries.contains_key(&id)
    }
    pub(super) fn new(owner: &Arc<Session>, limit: usize) -> Arc<Self> {
        Arc::new(Self {
            owner: Arc::downgrade(owner),
            slots: Arc::new(Semaphore::new(limit)),
            state: Mutex::new(State::default()),
        })
    }
    pub(super) fn insert(self: &Arc<Self>, exchange: Exchange) -> Option<Lease> {
        let slot = self.slots.clone().try_acquire_owned().ok()?;
        // Relaxed only allocates unique numbers; mutexes publish the actual entries.
        let id = NEXT
            .fetch_update(Ordering::Relaxed, Ordering::Relaxed, |id| {
                (id <= MAX_ID).then(|| id + 1)
            })
            .ok()?;
        let mut routes = ROUTES.lock().expect("request routes poisoned");
        let mut state = self.state.lock().expect("exchange table poisoned");
        if state.closed {
            return None;
        }
        state.entries.insert(id, exchange);
        routes.insert(id, Arc::downgrade(self));
        Some(Lease {
            id,
            table: self.clone(),
            _slot: slot,
        })
    }
    pub(super) fn take(
        owner: &Arc<Session>,
        id: u64,
        check: impl FnOnce(&Exchange) -> Result<(), AlefError>,
    ) -> Result<Exchange, AlefError> {
        let mut routes = ROUTES.lock().map_err(|_| missing())?;
        let table = routes
            .get(&id)
            .and_then(Weak::upgrade)
            .ok_or_else(missing)?;
        if !table.owner.ptr_eq(&Arc::downgrade(owner)) || !owner.is_open() {
            return Err(missing());
        }
        let mut state = table.state.lock().map_err(|_| missing())?;
        check(state.entries.get(&id).ok_or_else(missing)?)?;
        let exchange = state.entries.remove(&id).ok_or_else(missing)?;
        routes.remove(&id);
        Ok(exchange)
    }
}
impl Drop for Lease {
    fn drop(&mut self) {
        // Cleanup must not panic during unwinding. Poisoned maps are still structurally valid;
        // remove is idempotent even if a checking callback panicked before any mutation.
        let mut routes = ROUTES.lock().unwrap_or_else(|e| e.into_inner());
        let mut state = self.table.state.lock().unwrap_or_else(|e| e.into_inner());
        routes.remove(&self.id);
        state.entries.remove(&self.id);
    }
}
impl Drop for Closing {
    fn drop(&mut self) {
        let mut routes = ROUTES.lock().unwrap_or_else(|e| e.into_inner());
        let mut state = self.0.state.lock().unwrap_or_else(|e| e.into_inner());
        state.closed = true;
        for id in state.entries.keys() {
            routes.remove(id);
        }
        state.entries.clear();
    }
}
