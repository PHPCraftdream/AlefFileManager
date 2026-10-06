// SPDX-License-Identifier: MIT OR Apache-2.0
//! Per-session resource ownership and explicit asynchronous teardown.
use std::{future::Future, pin::Pin, sync::Mutex};

use crate::{
    error::{AlefError, ErrorCode},
    ids::ResourceId,
};

/// Session-owned handle (file, socket, process...) with explicit asynchronous close.
/// Boxing the future keeps this trait object-safe; consuming `Box<Self>` prevents use after close.
pub trait Resource: Send + 'static {
    /// Closes the resource, consuming it.
    fn close(self: Box<Self>) -> Pin<Box<dyn Future<Output = ()> + Send>>;
}

struct State {
    slots: Vec<(ResourceId, Option<Box<dyn Resource>>)>,
    next_id: u64,
    closed: bool,
}

/// Per-session table whose limit counts live resources; zero rejects every insertion as busy.
pub struct ResourceTable {
    limit: usize,
    state: Mutex<State>,
}

impl ResourceTable {
    /// Creates an empty resource table with a live-resource limit.
    pub fn new(limit: usize) -> Self {
        Self {
            limit,
            state: Mutex::new(State {
                slots: Vec::new(),
                next_id: 1,
                closed: false,
            }),
        }
    }
    /// Returns the live-resource limit.
    pub fn limit(&self) -> usize {
        self.limit
    }
    /// Inserts a resource; returns Busy at the limit and Closed after teardown.
    pub fn insert(&self, resource: Box<dyn Resource>) -> Result<ResourceId, AlefError> {
        let mut state = self.state.lock().expect("resource mutex poisoned");
        if state.closed {
            return Err(AlefError::new(ErrorCode::Closed, "resource table closed"));
        }
        if state
            .slots
            .iter()
            .filter(|(_, value)| value.is_some())
            .count()
            >= self.limit
        {
            return Err(AlefError::new(ErrorCode::Busy, "resource limit reached"));
        }
        let id = ResourceId(state.next_id);
        state.next_id = state
            .next_id
            .checked_add(1)
            .ok_or_else(|| AlefError::new(ErrorCode::Internal, "resource id exhausted"))?;
        state.slots.push((id, Some(resource)));
        Ok(id)
    }
    /// Runs `f` while the resource remains borrowed; returns NotFound if absent.
    pub fn with<R>(
        &self,
        id: ResourceId,
        f: impl FnOnce(&dyn Resource) -> R,
    ) -> Result<R, AlefError> {
        let state = self.state.lock().expect("resource mutex poisoned");
        let resource = state
            .slots
            .iter()
            .find(|(key, _)| *key == id)
            .and_then(|(_, value)| value.as_deref())
            .ok_or_else(|| AlefError::new(ErrorCode::NotFound, "resource not found"))?;
        Ok(f(resource))
    }
    /// Removes and returns ownership of a resource.
    pub fn take(&self, id: ResourceId) -> Result<Box<dyn Resource>, AlefError> {
        let mut state = self.state.lock().expect("resource mutex poisoned");
        state
            .slots
            .iter_mut()
            .find(|(key, _)| *key == id)
            .and_then(|(_, value)| value.take())
            .ok_or_else(|| AlefError::new(ErrorCode::NotFound, "resource not found"))
    }
    /// Drops the resource without awaiting close; use `take` or `close_all` for asynchronous close.
    pub fn remove(&self, id: ResourceId) -> Result<(), AlefError> {
        drop(self.take(id)?);
        Ok(())
    }
    /// Closes all inserted resources once, in reverse insertion order, then seals the table.
    pub async fn close_all(&self) {
        let resources = {
            let mut state = self.state.lock().expect("resource mutex poisoned");
            if state.closed {
                return;
            }
            state.closed = true;
            state
                .slots
                .iter_mut()
                .rev()
                .filter_map(|(_, resource)| resource.take())
                .collect::<Vec<_>>()
        };
        for resource in resources {
            resource.close().await;
        }
    }
    /// Returns the number of live resources.
    pub fn len(&self) -> usize {
        self.state
            .lock()
            .expect("resource mutex poisoned")
            .slots
            .iter()
            .filter(|(_, v)| v.is_some())
            .count()
    }
    /// Reports whether the table contains no live resources.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
    /// Reports whether teardown has sealed the table.
    pub fn is_closed(&self) -> bool {
        self.state.lock().expect("resource mutex poisoned").closed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};

    struct Recording {
        name: &'static str,
        log: Arc<Mutex<Vec<&'static str>>>,
    }
    impl Resource for Recording {
        fn close(self: Box<Self>) -> Pin<Box<dyn Future<Output = ()> + Send>> {
            Box::pin(async move {
                self.log.lock().expect("log mutex poisoned").push(self.name);
            })
        }
    }
    fn resource(name: &'static str, log: &Arc<Mutex<Vec<&'static str>>>) -> Box<dyn Resource> {
        Box::new(Recording {
            name,
            log: log.clone(),
        })
    }
    fn error_code<T>(result: Result<T, AlefError>, code: ErrorCode) {
        match result {
            Err(error) => assert_eq!(error.code, code),
            Ok(_) => panic!("expected error"),
        }
    }

    #[tokio::test]
    async fn closes_in_reverse_once_and_rejects_insert_after_close() {
        let table = ResourceTable::new(3);
        let log = Arc::new(Mutex::new(Vec::new()));
        table.insert(resource("a", &log)).expect("insert");
        table.insert(resource("b", &log)).expect("insert");
        timeout_close(&table).await;
        timeout_close(&table).await;
        assert_eq!(*log.lock().expect("log"), vec!["b", "a"]);
        error_code(table.insert(resource("c", &log)), ErrorCode::Closed);
    }
    #[tokio::test]
    async fn taken_resource_is_not_closed_and_ids_are_never_reused() {
        let table = ResourceTable::new(3);
        let log = Arc::new(Mutex::new(Vec::new()));
        let a = table.insert(resource("a", &log)).expect("insert");
        let b = table.insert(resource("b", &log)).expect("insert");
        drop(table.take(a).expect("take"));
        let c = table.insert(resource("c", &log)).expect("insert");
        assert!(c.0 > b.0 && c != a);
        timeout_close(&table).await;
        assert_eq!(*log.lock().expect("log"), vec!["c", "b"]);
    }
    #[test]
    fn enforces_limit_and_zero_limit() {
        let log = Arc::new(Mutex::new(Vec::new()));
        let table = ResourceTable::new(1);
        let id = table.insert(resource("a", &log)).expect("insert");
        error_code(table.insert(resource("b", &log)), ErrorCode::Busy);
        table.remove(id).expect("remove");
        assert_eq!(table.len(), 0);
        assert!(table.is_empty());
        assert!(table.insert(resource("c", &log)).is_ok());
        error_code(
            ResourceTable::new(0).insert(resource("z", &log)),
            ErrorCode::Busy,
        );
    }
    #[test]
    fn unknown_operations_and_with_value() {
        let table = ResourceTable::new(1);
        let id = ResourceId(99);
        error_code(table.take(id), ErrorCode::NotFound);
        error_code(table.with(id, |_| ()), ErrorCode::NotFound);
        error_code(table.remove(id), ErrorCode::NotFound);
        let log = Arc::new(Mutex::new(Vec::new()));
        let known = table.insert(resource("x", &log)).expect("insert");
        assert_eq!(table.with(known, |_| 42).expect("with"), 42);
        assert_eq!(table.limit(), 1);
    }
    async fn timeout_close(table: &ResourceTable) {
        tokio::time::timeout(std::time::Duration::from_secs(10), table.close_all())
            .await
            .expect("must not time out");
    }
}
