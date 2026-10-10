// SPDX-License-Identifier: MIT OR Apache-2.0
//! One reader, bounded owned queue, and stop-aware join. Reusable by future tray integration.
use crossbeam_channel::{bounded, Receiver, RecvTimeoutError, TrySendError};
use std::{
    io,
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
    thread::JoinHandle,
    time::Duration,
};

const IDLE: Duration = Duration::from_millis(50);
const CAPACITY: usize = 256;

pub(crate) struct Forwarder<T> {
    events: Receiver<T>,
    stop: Arc<AtomicBool>,
    thread: Option<JoinHandle<()>>,
}

impl<T: Send + 'static> Forwarder<T> {
    pub(crate) fn start(
        source: Receiver<T>,
        wake: impl Fn() -> bool + Send + 'static,
    ) -> io::Result<Self> {
        let (out, events) = bounded(CAPACITY);
        let stop = Arc::new(AtomicBool::new(false));
        let flag = stop.clone();
        let thread = std::thread::Builder::new()
            .name("alef-integration-forward".into())
            .spawn(move || {
                while !flag.load(Ordering::Acquire) {
                    let event = match source.recv_timeout(IDLE) {
                        Ok(event) => event,
                        Err(RecvTimeoutError::Timeout) => continue,
                        Err(RecvTimeoutError::Disconnected) => break,
                    };
                    match out.try_send(event) {
                        Ok(()) | Err(TrySendError::Full(_)) => {} // non-durable: drop newest on overflow
                        Err(TrySendError::Disconnected(_)) => break,
                    }
                    if !wake() {
                        break;
                    }
                }
            })?;
        Ok(Self {
            events,
            stop,
            thread: Some(thread),
        })
    }

    pub(crate) fn drain(&self) -> impl Iterator<Item = T> + '_ {
        self.events.try_iter().take(CAPACITY)
    }

    /// The next event, waiting for it up to `within`.
    #[cfg(any(windows, target_os = "macos"))]
    pub(crate) fn next_within(&self, within: Duration) -> Option<T> {
        self.events.recv_timeout(within).ok()
    }
}

impl<T> Drop for Forwarder<T> {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Release);
        if let Some(thread) = self.thread.take() {
            let _ = thread.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    const WAIT: Duration = Duration::from_secs(2);

    fn bounded_drop<T: Send + 'static>(forward: Forwarder<T>) {
        let (done, completion) = bounded(1);
        // Detach the observer so a broken Drop fails at the deadline rather than hanging the test.
        let _ = std::thread::spawn(move || {
            drop(forward);
            done.send_timeout((), WAIT).unwrap();
        });
        completion
            .recv_timeout(WAIT)
            .expect("forwarder drop must complete");
    }

    #[test]
    fn owned_events_are_forwarded_in_order_and_idle_drop_joins() {
        let (send, receive) = bounded(8);
        let (woken, wakes) = bounded(8);
        let forward =
            Forwarder::start(receive, move || woken.send_timeout((), WAIT).is_ok()).unwrap();
        send.send_timeout(String::from("one"), WAIT).unwrap();
        send.send_timeout(String::from("two"), WAIT).unwrap();
        wakes.recv_timeout(Duration::from_secs(2)).unwrap();
        wakes.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(forward.drain().collect::<Vec<_>>(), ["one", "two"]);
        bounded_drop(forward);
    }
    #[cfg(any(windows, target_os = "macos"))]
    #[test]
    fn next_within_waits_for_an_event_and_gives_up_without_one() {
        let (send, receive) = bounded(8);
        let forward = Forwarder::start(receive, || true).unwrap();
        assert_eq!(forward.next_within(Duration::from_millis(20)), None::<u8>);
        send.send_timeout(7_u8, WAIT).unwrap();
        assert_eq!(forward.next_within(WAIT), Some(7));
        assert_eq!(forward.next_within(Duration::from_millis(20)), None);
        bounded_drop(forward);
    }
    #[test]
    fn overflow_does_not_block_shutdown() {
        let (send, receive) = bounded(CAPACITY * 2);
        let (done, notified) = bounded(1);
        let mut count = 0;
        // Atomic counter is used because the public wake closure is Fn.
        let seen = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let counter = seen.clone();
        let forward = Forwarder::start(receive, move || {
            if counter.fetch_add(1, Ordering::Relaxed) + 1 == CAPACITY * 2 {
                let _ = done.try_send(());
            }
            true
        })
        .unwrap();
        while count < CAPACITY * 2 {
            send.send_timeout(count, WAIT).unwrap();
            count += 1;
        }
        notified.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(forward.drain().count(), CAPACITY);
        bounded_drop(forward);
    }
}
