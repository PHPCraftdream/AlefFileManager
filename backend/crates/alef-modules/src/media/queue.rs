// SPDX-License-Identifier: MIT OR Apache-2.0
//! Bounded drop-oldest frame queues: a live producer never waits for a slow consumer, the queue
//! keeps the newest frames and counts what it dropped. Items are whole frames; the drop-oldest
//! policy never tears one apart, and each item travels as a JSON header plus binary chunks whose
//! lengths sum to the header's `byteLength`.
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

/// Milliseconds since the Unix epoch; zero when the clock is before it.
pub fn unix_ms() -> u64 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |d| d.as_millis() as u64)
}

#[derive(Debug)]
struct State<T> {
    items: VecDeque<T>,
    closed: bool,
}

/// A queue that keeps the newest `capacity` items and never blocks the producer. Closing it ends
/// the iteration: closed wins over queued items, so the items left in it are never read.
#[derive(Debug)]
pub struct MediaQueue<T> {
    state: Mutex<State<T>>,
    notify: Notify,
    capacity: usize,
    pushed: AtomicU64,
    dropped: AtomicU64,
}

impl<T> MediaQueue<T> {
    /// Creates a queue of the given capacity; zero is a queue without room for anything.
    pub fn new(capacity: usize) -> Self {
        assert!(capacity > 0);
        Self {
            state: Mutex::new(State {
                items: VecDeque::new(),
                closed: false,
            }),
            notify: Notify::new(),
            capacity,
            pushed: AtomicU64::new(0),
            dropped: AtomicU64::new(0),
        }
    }

    /// Queues an item, dropping the oldest one when the queue is full.
    pub fn push(&self, item: T) {
        self.pushed.fetch_add(1, Ordering::Relaxed);
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.items.len() >= self.capacity {
            state.items.pop_front();
            self.dropped.fetch_add(1, Ordering::Relaxed);
        }
        state.items.push_back(item);
        drop(state);
        self.notify.notify_one();
    }

    /// Ends the queue; the items left in it are never read.
    pub fn close(&self) {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).closed = true;
        self.notify.notify_one();
    }

    /// How many items the queue dropped for being full.
    pub fn drops(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// How many items were ever pushed, dropped ones included.
    pub fn pushed(&self) -> u64 {
        self.pushed.load(Ordering::Relaxed)
    }
}

/// What one lock-and-check of the queue found.
enum Popped<T> {
    /// The next queued item.
    Item(T),
    /// Open, nothing queued yet: `pop` waits.
    Empty,
    /// Closed: the iteration ends, items left behind are never read.
    Closed,
}

impl<T> MediaQueue<T> {
    /// One lock-and-check of the queue. Keeping the guard out of the async body keeps the `pop`
    /// future `Send`.
    fn try_pop(&self) -> Popped<T> {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.closed {
            return Popped::Closed;
        }
        match state.items.pop_front() {
            Some(item) => Popped::Item(item),
            None => Popped::Empty,
        }
    }

    /// Takes the next item; `None` once the queue is closed, items left behind included.
    pub async fn pop(&self) -> Option<T> {
        let notified = self.notify.notified();
        tokio::pin!(notified);
        loop {
            if let Popped::Item(item) = self.try_pop() {
                return Some(item);
            }
            notified.as_mut().enable();
            // The push (or the close) may have happened between the check and the arming.
            match self.try_pop() {
                Popped::Item(item) => return Some(item),
                Popped::Closed => return None,
                Popped::Empty => {}
            }
            notified.as_mut().await;
            notified.set(self.notify.notified());
        }
    }
}

/// An item of a media stream: a JSON header the page reads first, then the binary payload, sent
/// as chunks whose lengths sum to the header's `byteLength`.
pub trait StreamItem: Send + 'static {
    /// The header frame of the item.
    fn header(&self) -> serde_json::Value;
    /// The payload frame of the item.
    fn payload(&self) -> Bytes;
}

/// One frame of a camera: RGBA pixels, row after row.
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
        serde_json::json!({
            "seq": self.seq,
            "timestamp": self.timestamp_ms,
            "width": self.width,
            "height": self.height,
            "dropped": self.dropped,
            "format": "rgba",
            "byteLength": self.payload.len(),
        })
    }

    fn payload(&self) -> Bytes {
        self.payload.clone()
    }
}

/// One chunk of a microphone: little-endian `f32` samples. The payload travels as binary chunks
/// summing to `byteLength`.
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
        serde_json::json!({
            "seq": self.seq,
            "timestamp": self.timestamp_ms,
            "rate": self.rate,
            "samples": self.samples,
            "dropped": self.dropped,
            "format": "f32le",
            "byteLength": self.payload.len(),
        })
    }

    fn payload(&self) -> Bytes {
        self.payload.clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

    async fn bounded<T>(future: impl std::future::Future<Output = T>) -> T {
        tokio::time::timeout(Duration::from_secs(2), future)
            .await
            .expect("the pop must not hang")
    }

    #[tokio::test]
    async fn push_beyond_capacity_drops_the_oldest_and_counts() {
        let queue = MediaQueue::new(2);
        queue.push(1);
        queue.push(2);
        queue.push(3);
        let first = bounded(queue.pop()).await;
        let second = bounded(queue.pop()).await;
        assert_eq!(first, Some(2));
        assert_eq!(second, Some(3));
        assert_eq!(queue.drops(), 1);
        assert_eq!(queue.pushed(), 3);
    }

    #[tokio::test]
    async fn a_closed_queue_ends_the_iteration_and_ignores_later_items() {
        let queue = MediaQueue::new(2);
        queue.push(1);
        queue.close();
        let first = bounded(queue.pop()).await;
        assert_eq!(first, None);
        queue.push(2);
        let second = bounded(queue.pop()).await;
        assert_eq!(second, None);
    }

    #[tokio::test]
    async fn pop_wakes_when_an_item_arrives() {
        let queue = std::sync::Arc::new(MediaQueue::new(1));
        let waiter = {
            let queue = queue.clone();
            tokio::spawn(async move { queue.pop().await })
        };
        tokio::time::sleep(Duration::from_millis(10)).await;
        queue.push(7);
        let item = tokio::time::timeout(Duration::from_secs(2), waiter)
            .await
            .expect("the pop must not hang")
            .expect("the task must not fail");
        assert_eq!(item, Some(7));
    }
}
