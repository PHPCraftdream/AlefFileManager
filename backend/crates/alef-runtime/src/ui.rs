// SPDX-License-Identifier: MIT OR Apache-2.0
use std::io;
use std::sync::Arc;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use tokio::sync::{mpsc, oneshot, watch, OwnedSemaphorePermit, Semaphore};
use winit::event_loop::EventLoopProxy;

use crate::bridge::EventBus;

pub(crate) const WINDOW_STATE_EVENT: &str = "runtime.window.state";
const UI_CAPACITY: usize = 64;
const MAX_EVENT_BYTES: usize = 256 * 1024;

#[derive(Clone, Debug)]
pub(crate) struct Wake;

#[derive(Clone)]
pub struct RuntimeHandle {
    sender: mpsc::Sender<UiRequest>,
    events: EventBus,
    proxy: watch::Sender<Option<EventLoopProxy<Wake>>>,
    admission: Arc<Semaphore>,
}

#[derive(Clone, Copy, Debug, Deserialize)]
#[serde(rename_all = "camelCase")]
pub enum ResizeEdge {
    North,
    NorthEast,
    East,
    SouthEast,
    South,
    SouthWest,
    West,
    NorthWest,
}

#[derive(Debug, Deserialize)]
#[serde(tag = "action", rename_all = "camelCase", deny_unknown_fields)]
pub enum WindowAction {
    GetState,
    Minimize,
    Maximize,
    Restore,
    ToggleMaximize,
    Close,
    SetDecorations { enabled: bool },
    SetResizable { enabled: bool },
    StartDrag,
    StartResize { edge: ResizeEdge },
}

#[derive(Clone, Debug, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct WindowState {
    pub revision: u32,
    pub title: String,
    pub width: u32,
    pub height: u32,
    pub x: Option<i32>,
    pub y: Option<i32>,
    pub scale_factor: f64,
    pub focused: bool,
    pub maximized: bool,
    pub minimized: Option<bool>,
    pub visible: Option<bool>,
    pub decorated: bool,
    pub resizable: bool,
    pub fullscreen: bool,
    pub supports_drag_resize: bool,
}

pub(crate) enum UiRequest {
    Window {
        action: WindowAction,
        reply: UiReply,
    },
}

pub(crate) struct UiReply {
    sender: oneshot::Sender<io::Result<Value>>,
    _permit: OwnedSemaphorePermit,
}

impl UiReply {
    pub(crate) fn canceled(&self) -> bool {
        self.sender.is_closed()
    }
    pub(crate) fn finish(self, result: io::Result<Value>) {
        let _ = self.sender.send(result);
    }
}

impl RuntimeHandle {
    pub(crate) fn channel() -> (Self, mpsc::Receiver<UiRequest>) {
        let (sender, receiver) = mpsc::channel(UI_CAPACITY);
        let (proxy, _) = watch::channel(None);
        (
            Self {
                sender,
                events: EventBus::default(),
                proxy,
                admission: Arc::new(Semaphore::new(UI_CAPACITY)),
            },
            receiver,
        )
    }

    /// Document event streams (`runtime.events.subscribe`).
    pub(crate) fn events(&self) -> &EventBus {
        &self.events
    }

    pub(crate) fn attach(&self, proxy: EventLoopProxy<Wake>) {
        self.proxy.send_replace(Some(proxy));
    }
    pub(crate) fn detach(&self) {
        self.proxy.send_replace(None);
        self.admission.close();
    }

    /// Non-durable broadcast to the event streams of every document; returns once queued.
    pub async fn emit<T: Serialize + Sync + ?Sized>(
        &self,
        name: &str,
        payload: &T,
    ) -> io::Result<()> {
        if name.starts_with("runtime.") {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "The runtime event namespace is reserved",
            ));
        }
        if self.admission.is_closed() {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "Native window is closed",
            ));
        }
        self.events.publish(None, &event_json(name, payload)?);
        Ok(())
    }

    pub async fn window(&self, action: WindowAction) -> io::Result<Value> {
        self.request(move |reply| Ok(UiRequest::Window { action, reply }))
            .await
    }

    /// Cancel-safe before execution; an already applied OS operation is not rolled back.
    async fn request(
        &self,
        build: impl FnOnce(UiReply) -> io::Result<UiRequest>,
    ) -> io::Result<Value> {
        if self.admission.is_closed() {
            return Err(io::Error::new(
                io::ErrorKind::BrokenPipe,
                "Native window is closed",
            ));
        }
        let proxy = self.proxy.borrow().clone().ok_or_else(|| {
            io::Error::new(io::ErrorKind::NotConnected, "Native window is not attached")
        })?;
        let permit =
            self.admission.clone().acquire_owned().await.map_err(|_| {
                io::Error::new(io::ErrorKind::BrokenPipe, "Native window is closed")
            })?;
        let (sender, response) = oneshot::channel();
        self.sender
            .send(build(UiReply {
                sender,
                _permit: permit,
            })?)
            .await
            .map_err(|_| io::Error::new(io::ErrorKind::BrokenPipe, "Native window is closed"))?;
        proxy.send_event(Wake).map_err(|_| {
            io::Error::new(io::ErrorKind::BrokenPipe, "Native event loop is closed")
        })?;
        response.await.map_err(|_| {
            io::Error::new(
                io::ErrorKind::BrokenPipe,
                "Native window closed before completion",
            )
        })?
    }
}

pub(crate) fn event_json<T: Serialize + ?Sized>(name: &str, payload: &T) -> io::Result<String> {
    #[derive(Serialize)]
    struct Event<'a, T: ?Sized> {
        name: &'a str,
        payload: &'a T,
    }
    if name.is_empty() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Event name is empty",
        ));
    }
    let json = serde_json::to_string(&Event { name, payload }).map_err(io::Error::other)?;
    if json.len() > MAX_EVENT_BYTES {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "Event exceeds 256 KiB",
        ));
    }
    Ok(json)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn native_operations_fail_without_an_attached_window() {
        let (handle, _receiver) = RuntimeHandle::channel();
        handle
            .emit("example", &1)
            .await
            .expect("events need no attached window");
        assert_eq!(
            handle
                .window(WindowAction::GetState)
                .await
                .expect_err("unattached")
                .kind(),
            io::ErrorKind::NotConnected
        );
    }

    #[tokio::test]
    async fn retained_backend_handle_fails_after_window_shutdown() {
        let (handle, _receiver) = RuntimeHandle::channel();
        let backend = handle.clone();
        handle.detach();
        assert_eq!(
            backend
                .emit("example", &1)
                .await
                .expect_err("closed")
                .kind(),
            io::ErrorKind::BrokenPipe
        );
        assert_eq!(
            backend
                .window(WindowAction::GetState)
                .await
                .expect_err("closed")
                .kind(),
            io::ErrorKind::BrokenPipe
        );
    }

    #[tokio::test]
    async fn emitted_events_reach_event_streams_and_the_runtime_namespace_is_reserved() {
        use alef_core::{
            protocol::{call::Limits, frame::Frame},
            session::{session::SessionManager, TokenSource},
        };
        let (handle, _receiver) = RuntimeHandle::channel();
        let source: TokenSource = Arc::new(|| "token".to_owned());
        let sessions = SessionManager::new(source, Limits::default());
        let session = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            sessions.begin_document(1),
        )
        .await
        .expect("session");
        let (writer, id) = session.streams().open_outgoing();
        handle.events().attach(1, writer);
        let mut reader = session.streams().reader(id).expect("reader");

        handle
            .emit("backend.greeting", &serde_json::json!({"n": 1}))
            .await
            .expect("emit");
        let frame = tokio::time::timeout(std::time::Duration::from_secs(10), reader.next_frame())
            .await
            .expect("event in time");
        match frame {
            Some(Frame::Json(value)) => assert_eq!(
                value,
                serde_json::json!({"name": "backend.greeting", "payload": {"n": 1}})
            ),
            other => panic!("expected the event as a json frame, got {other:?}"),
        }
        for reserved in ["runtime.window.state", "runtime."] {
            let error = handle.emit(reserved, &1).await.expect_err("reserved");
            assert_eq!(error.kind(), io::ErrorKind::InvalidInput, "{reserved}");
        }
    }

    #[test]
    fn oversized_event_is_rejected() {
        let error =
            event_json("example", &"x".repeat(MAX_EVENT_BYTES)).expect_err("oversized event");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }
}
