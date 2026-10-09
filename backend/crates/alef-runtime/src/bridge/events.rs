// SPDX-License-Identifier: MIT OR Apache-2.0
//! Document events as a stream: `runtime.events.subscribe` opens one outgoing stream per
//! subscriber; the window host publishes `{ "name", "payload" }` JSON to it without ever blocking.
use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc, Mutex,
};

use alef_core::{
    error::{AlefError, ErrorCode},
    ids::SessionId,
    registry::{command::Reply, dispatch::Registry},
    session::streams::StreamWriter,
};
use serde_json::Value;
use tokio::sync::mpsc;

/// Events buffered per subscriber before it counts as too slow.
const QUEUE: usize = 256;

struct Subscriber {
    window: u64,
    session: Option<SessionId>,
    queue: mpsc::Sender<Arc<Value>>,
    overflowed: Arc<AtomicBool>,
}

/// Fan-out of runtime events to subscribed documents.
///
/// Policy for a subscriber that does not keep up (its credit window is exhausted and its queue of
/// [`QUEUE`] events is full): it is dropped and its stream ends with `BUSY`. The UI thread never waits.
#[derive(Clone, Default)]
pub struct EventBus {
    subscribers: Arc<Mutex<Vec<Subscriber>>>,
}

impl EventBus {
    /// Starts delivering events of `window` (`None` = every window's events) to `writer`.
    #[cfg(test)]
    pub(crate) fn attach(&self, window: u64, writer: StreamWriter) {
        self.attach_session(window, None, writer);
    }

    fn attach_session(&self, window: u64, session: Option<SessionId>, writer: StreamWriter) {
        let (queue, mut events) = mpsc::channel::<Arc<Value>>(QUEUE);
        let overflowed = Arc::new(AtomicBool::new(false));
        let flag = overflowed.clone();
        tokio::spawn(async move {
            while let Some(event) = events.recv().await {
                if writer.send_json((*event).clone()).await.is_err() {
                    return; // closed by the page or by the end of the session
                }
            }
            if flag.load(Ordering::SeqCst) {
                writer.error(AlefError::new(ErrorCode::Busy, "event subscriber too slow"));
            } else {
                writer.end();
            }
        });
        self.subscribers
            .lock()
            .expect("event subscribers")
            .push(Subscriber {
                window,
                session,
                queue,
                overflowed,
            });
    }

    /// Publishes one event (already serialized as `{ "name", "payload" }`) to the subscribers of
    /// `window`, or of every window when `None`. Never blocks.
    pub(crate) fn publish(&self, window: Option<u64>, json: &str) {
        self.publish_target(window, None, json);
    }

    pub(crate) fn publish_session(&self, window: u64, session: SessionId, json: &str) {
        self.publish_target(Some(window), Some(session), json);
    }

    fn publish_target(&self, window: Option<u64>, session: Option<SessionId>, json: &str) {
        let Ok(event) = serde_json::from_str::<Value>(json) else {
            return;
        };
        let event = Arc::new(event);
        let mut subscribers = self.subscribers.lock().expect("event subscribers");
        subscribers.retain(|subscriber| {
            if window.is_some_and(|window| window != subscriber.window)
                || session.is_some_and(|session| Some(session) != subscriber.session)
            {
                return true;
            }
            match subscriber.queue.try_send(event.clone()) {
                Ok(()) => true,
                Err(mpsc::error::TrySendError::Full(_)) => {
                    subscriber.overflowed.store(true, Ordering::SeqCst);
                    false
                }
                Err(mpsc::error::TrySendError::Closed(_)) => false,
            }
        });
    }

    /// Number of live subscribers.
    #[cfg(test)]
    pub(crate) fn subscribers(&self) -> usize {
        self.subscribers.lock().expect("event subscribers").len()
    }

    /// Registers `runtime.events.subscribe`: opens an outgoing stream in the caller's session.
    pub(crate) fn register(&self, registry: &mut Registry) -> Result<(), AlefError> {
        let bus = self.clone();
        registry
            .register_runtime::<Value>("runtime.events.subscribe")?
            .handler(move |ctx, _| {
                let bus = bus.clone();
                async move {
                    let (writer, id) = ctx.streams().open_outgoing();
                    bus.attach_session(ctx.session.window(), Some(ctx.session.id()), writer);
                    Ok(Reply::Stream(id))
                }
            })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alef_core::{
        protocol::{call::Limits, frame::Frame},
        session::session::SessionManager,
    };
    use serde_json::json;
    use std::time::Duration;

    async fn bounded<T>(future: impl std::future::Future<Output = T>) -> T {
        tokio::time::timeout(Duration::from_secs(10), future)
            .await
            .expect("timed out")
    }

    async fn session(window: u64) -> Arc<alef_core::session::Session> {
        let source: alef_core::session::TokenSource = Arc::new(|| "token".to_owned());
        let sessions = SessionManager::new(source, Limits::default());
        bounded(sessions.begin_document(window)).await
    }

    fn subscribe(
        bus: &EventBus,
        session: &alef_core::session::Session,
    ) -> alef_core::session::streams::StreamReader {
        let (writer, id) = session.streams().open_outgoing();
        bus.attach(session.window(), writer);
        session.streams().reader(id).expect("reader")
    }

    async fn next_json(reader: &mut alef_core::session::streams::StreamReader) -> Value {
        match bounded(reader.next_frame()).await {
            Some(Frame::Json(value)) => value,
            other => panic!("expected a json frame, got {other:?}"),
        }
    }

    #[tokio::test]
    async fn subscribers_get_events_in_order_and_only_their_windows() {
        let bus = EventBus::default();
        let (one, two) = (session(1).await, session(2).await);
        let (mut a, mut b) = (subscribe(&bus, &one), subscribe(&bus, &two));
        bus.publish(Some(1), r#"{"name":"first","payload":1}"#);
        bus.publish(None, r#"{"name":"second","payload":2}"#);
        bus.publish(Some(2), r#"{"name":"third","payload":3}"#);
        assert_eq!(next_json(&mut a).await["name"], "first");
        assert_eq!(next_json(&mut a).await["name"], "second");
        assert_eq!(next_json(&mut b).await["name"], "second");
        assert_eq!(next_json(&mut b).await["name"], "third");
        bus.publish(None, "not json");
        bus.publish(None, r#"{"name":"after","payload":null}"#);
        assert_eq!(
            next_json(&mut a).await["name"],
            "after",
            "garbage is ignored"
        );
    }

    #[tokio::test]
    async fn session_target_does_not_deliver_to_another_document_in_the_same_window() {
        let bus = EventBus::default();
        let document = session(1).await;
        let (writer, id) = document.streams().open_outgoing();
        bus.attach_session(1, Some(document.id()), writer);
        let mut reader = document.streams().reader(id).expect("reader");
        bus.publish_session(
            1,
            SessionId(document.id().0 + 1),
            r#"{"name":"stale","payload":null}"#,
        );
        bus.publish_session(
            2,
            document.id(),
            r#"{"name":"wrong-window","payload":null}"#,
        );
        bus.publish_session(1, document.id(), r#"{"name":"owned","payload":1}"#);
        bus.publish(Some(1), r#"{"name":"sentinel","payload":null}"#);
        assert_eq!(next_json(&mut reader).await["name"], "owned");
        assert_eq!(next_json(&mut reader).await["name"], "sentinel");
    }

    #[tokio::test]
    async fn a_closed_stream_unsubscribes() {
        let bus = EventBus::default();
        let session = session(1).await;
        let (writer, id) = session.streams().open_outgoing();
        bus.attach(1, writer);
        assert_eq!(bus.subscribers(), 1);
        session.streams().close(id).expect("close");
        bus.publish(None, r#"{"name":"x","payload":0}"#);
        bounded(async {
            while bus.subscribers() != 0 {
                bus.publish(None, r#"{"name":"x","payload":0}"#);
                tokio::task::yield_now().await;
            }
        })
        .await;
    }

    #[tokio::test]
    async fn a_stalled_subscriber_is_dropped_with_busy_and_publishing_never_blocks() {
        let bus = EventBus::default();
        let limits = Limits {
            stream_window: 64,
            chunk_size: 64,
            ..Limits::default()
        };
        let source: alef_core::session::TokenSource = Arc::new(|| "t".to_owned());
        let sessions = SessionManager::new(source, limits);
        let session = bounded(sessions.begin_document(1)).await;
        let mut reader = subscribe(&bus, &session);
        let event = format!(r#"{{"name":"big","payload":"{}"}}"#, "x".repeat(20));
        let started = std::time::Instant::now();
        for _ in 0..(QUEUE * 3) {
            bus.publish(None, &event);
        }
        assert!(
            started.elapsed() < Duration::from_secs(2),
            "publish must not wait for the page"
        );
        assert_eq!(bus.subscribers(), 0, "the stalled subscriber was dropped");
        // the page finally reads (and acks): it still gets a clean terminal Busy error
        let mut last = None;
        bounded(async {
            while let Some(frame) = reader.next_frame().await {
                if let Frame::Json(_) = frame {
                    session
                        .streams()
                        .ack(reader.id(), 64)
                        .expect("ack keeps the pump moving");
                }
                last = Some(frame);
            }
        })
        .await;
        assert!(
            matches!(last, Some(Frame::Error(ref e)) if e.code == ErrorCode::Busy),
            "{last:?}"
        );
    }

    #[tokio::test]
    async fn the_subscribe_command_opens_a_stream_in_the_callers_session() {
        use alef_core::registry::context::CallContext;
        use alef_core::security::{
            manifest::Permissions,
            permissions::{PathVars, PermissionSet},
        };
        let bus = EventBus::default();
        let mut registry = Registry::default();
        bus.register(&mut registry).expect("register");
        let policy: Permissions = serde_json::from_value(json!({
            "fs": {"read": [], "write": []}, "cli": {"exec": []},
            "net": {"http": [], "socket": []}, "shell": {"openExternal": []},
            "clipboard": {"read": false}, "shortcut": {"global": false},
            "secrets": false, "app": {"env": []},
        }))
        .expect("policy");
        let root = std::env::current_dir().expect("cwd");
        let vars = PathVars {
            app_data: root.clone(),
            app_config: root.clone(),
            app_cache: root.clone(),
            home: root.clone(),
            documents: root.clone(),
            downloads: root.clone(),
            desktop: root.clone(),
            temp: root.clone(),
            app: root,
        };
        let permissions = Arc::new(PermissionSet::from_manifest(&policy, &vars).expect("set"));
        let session = session(7).await;
        let ctx = CallContext::new(session.clone(), permissions);
        let reply = bounded(registry.dispatch("runtime.events.subscribe", ctx, json!({})))
            .await
            .expect("subscribed");
        let Reply::Stream(id) = reply else {
            panic!("expected a stream reply");
        };
        let mut reader = session.streams().reader(id).expect("reader");
        bus.publish(Some(7), r#"{"name":"hello","payload":true}"#);
        assert_eq!(next_json(&mut reader).await["name"], "hello");
    }
}
