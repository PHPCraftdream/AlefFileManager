// SPDX-License-Identifier: MIT OR Apache-2.0
//! Bounded drop-oldest frame queues and their terminal producer state.
use alef_core::AlefError;
use bytes::Bytes;
use std::{
    collections::VecDeque,
    sync::{
        atomic::{AtomicU64, Ordering},
        Mutex,
    },
    time::{SystemTime, UNIX_EPOCH},
};
use tokio::sync::Notify;

pub fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}
#[derive(Debug)]
struct State<T> {
    items: VecDeque<T>,
    closed: bool,
    error: Option<AlefError>,
}
#[derive(Debug)]
pub struct MediaQueue<T> {
    state: Mutex<State<T>>,
    notify: Notify,
    capacity: usize,
    pushed: AtomicU64,
    dropped: AtomicU64,
    samples: AtomicU64,
}
impl<T> MediaQueue<T> {
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0);
        Self {
            state: Mutex::new(State {
                items: VecDeque::new(),
                closed: false,
                error: None,
            }),
            notify: Notify::new(),
            capacity,
            pushed: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
            samples: AtomicU64::new(0),
        }
    }
    pub fn close(&self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.closed = true;
        drop(state);
        self.notify.notify_waiters();
    }
    pub fn is_closed(&self) -> bool {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).closed
    }
    pub fn fail(&self, error: AlefError) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.error.is_none() {
            state.error = Some(error);
        }
        state.closed = true;
        drop(state);
        self.notify.notify_waiters();
    }
    pub fn error(&self) -> Option<AlefError> {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .error
            .clone()
    }
    pub async fn failed(&self) {
        let notified = self.notify.notified();
        tokio::pin!(notified);
        loop {
            if self.error().is_some() {
                return;
            }
            notified.as_mut().enable();
            if self.error().is_some() {
                return;
            }
            notified.as_mut().await;
            notified.set(self.notify.notified());
        }
    }
    pub fn add_drops(&self, count: u64) {
        self.dropped.fetch_add(count, Ordering::Relaxed);
    }
    pub fn drops(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }
    pub fn pushed(&self) -> u64 {
        self.pushed.load(Ordering::Relaxed)
    }
    pub fn samples(&self) -> u64 {
        self.samples.load(Ordering::Relaxed)
    }
}
impl<T: StreamItem> MediaQueue<T> {
    pub fn push(&self, item: T) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.closed || state.error.is_some() {
            return;
        }
        self.pushed.fetch_add(1, Ordering::Relaxed);
        if state.items.len() >= self.capacity {
            state.items.pop_front();
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
        self.samples
            .fetch_add(item.sample_count(), Ordering::Relaxed);
        state.items.push_back(item);
        drop(state);
        self.notify.notify_one();
    }
    fn try_pop(&self) -> Popped<T> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.closed {
            return if state.error.is_some() {
                Popped::Failed
            } else {
                Popped::Closed
            };
        }
        match state.items.pop_front() {
            Some(item) => Popped::Item(item),
            None => Popped::Empty,
        }
    }
    pub async fn pop(&self) -> Option<T> {
        let notified = self.notify.notified();
        tokio::pin!(notified);
        loop {
            if let Popped::Item(item) = self.try_pop() {
                return Some(item);
            }
            notified.as_mut().enable();
            match self.try_pop() {
                Popped::Item(item) => return Some(item),
                Popped::Closed | Popped::Failed => return None,
                Popped::Empty => {}
            }
            notified.as_mut().await;
            notified.set(self.notify.notified());
        }
    }
}
enum Popped<T> {
    Item(T),
    Empty,
    Closed,
    Failed,
}
pub trait StreamItem: Send + 'static {
    fn header(&self) -> serde_json::Value;
    fn payload(&self) -> Bytes;
    fn sample_count(&self) -> u64 {
        0
    }
}
#[derive(Debug)]
pub struct CameraFrame {
    pub width: u32,
    pub height: u32,
    pub seq: u64,
    pub timestamp_ms: u64,
    pub dropped: u64,
    pub payload: Bytes,
}
impl StreamItem for CameraFrame {
    fn header(&self) -> serde_json::Value {
        serde_json::json!({"seq":self.seq,"timestamp":self.timestamp_ms,"width":self.width,"height":self.height,"dropped":self.dropped,"format":"rgba","byteLength":self.payload.len()})
    }
    fn payload(&self) -> Bytes {
        self.payload.clone()
    }
}
#[derive(Debug)]
pub struct PcmChunk {
    pub seq: u64,
    pub timestamp_ms: u64,
    pub dropped: u64,
    pub rate: u32,
    pub samples: usize,
    pub payload: Bytes,
}
impl StreamItem for PcmChunk {
    fn header(&self) -> serde_json::Value {
        serde_json::json!({"seq":self.seq,"timestamp":self.timestamp_ms,"rate":self.rate,"samples":self.samples,"dropped":self.dropped,"format":"f32le","byteLength":self.payload.len()})
    }
    fn payload(&self) -> Bytes {
        self.payload.clone()
    }
    fn sample_count(&self) -> u64 {
        u64::try_from(self.samples).unwrap_or(u64::MAX)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    async fn bounded<T>(future: impl std::future::Future<Output = T>) -> T {
        tokio::time::timeout(Duration::from_secs(2), future)
            .await
            .expect("pop bounded")
    }
    fn frame(seq: u64) -> CameraFrame {
        CameraFrame {
            width: 1,
            height: 1,
            seq,
            timestamp_ms: 0,
            dropped: 0,
            payload: Bytes::new(),
        }
    }
    #[tokio::test]
    async fn push_beyond_capacity_drops_oldest_and_counts() {
        let q = MediaQueue::new(2);
        q.push(frame(1));
        q.push(frame(2));
        q.push(frame(3));
        assert_eq!(bounded(q.pop()).await.unwrap().seq, 2);
        assert_eq!(bounded(q.pop()).await.unwrap().seq, 3);
        assert_eq!(q.drops(), 1);
        assert_eq!(q.pushed(), 3);
    }
    #[tokio::test]
    async fn a_closed_queue_ends_and_ignores_later_items() {
        let q = MediaQueue::new(2);
        q.push(frame(1));
        q.close();
        assert!(bounded(q.pop()).await.is_none());
        q.push(frame(2));
        assert!(bounded(q.pop()).await.is_none());
    }
    #[tokio::test]
    async fn pop_wakes_when_an_item_arrives() {
        let q = std::sync::Arc::new(MediaQueue::new(1));
        let waiter = {
            let q = q.clone();
            tokio::spawn(async move { q.pop().await })
        };
        tokio::time::sleep(Duration::from_millis(10)).await;
        q.push(frame(7));
        assert_eq!(
            tokio::time::timeout(Duration::from_secs(2), waiter)
                .await
                .unwrap()
                .unwrap()
                .unwrap()
                .seq,
            7
        );
    }
}
