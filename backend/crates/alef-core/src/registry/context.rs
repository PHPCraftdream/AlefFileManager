// SPDX-License-Identifier: MIT OR Apache-2.0
//! Per-call context and cooperative cancellation.
use crate::{
    security::{
        consent::Decision,
        permissions::{Grants, PermissionSet},
    },
    session::{resources::ResourceTable, session::Session, streams::StreamHub},
};
use bytes::Bytes;
use std::sync::Arc;
use tokio::sync::watch;

/// Cooperative cancellation handle observed by handlers (tokio watch based).
#[derive(Clone)]
pub struct CancelHandle {
    receiver: watch::Receiver<bool>,
}
impl CancelHandle {
    /// Create a linked sender/handle pair.
    pub fn channel() -> (CancelSender, Self) {
        let (sender, receiver) = watch::channel(false);
        (
            CancelSender {
                sender: Arc::new(sender),
            },
            Self { receiver },
        )
    }
    /// Whether cancellation has been requested.
    pub fn is_cancelled(&self) -> bool {
        *self.receiver.borrow()
    }
    /// Resolve when cancelled; the future also resolves if the sender is dropped without cancelling.
    pub async fn cancelled(&self) {
        let mut receiver = self.receiver.clone();
        while !*receiver.borrow() {
            if receiver.changed().await.is_err() {
                return;
            }
        }
    }
}
/// Requesting side of a cancellation channel; clones share one channel.
#[derive(Clone)]
pub struct CancelSender {
    sender: Arc<watch::Sender<bool>>,
}
impl CancelSender {
    /// Request cancellation.
    pub fn cancel(&self) {
        self.sender.send_replace(true);
    }
    /// Whether cancellation has been requested.
    pub fn is_cancelled(&self) -> bool {
        *self.sender.borrow()
    }
}
/// Per-call authorization and payload context handed to command handlers.
#[derive(Clone)]
#[non_exhaustive]
pub struct CallContext {
    /// Owning document session (grants, resources, streams).
    pub session: Arc<Session>,
    /// Resolved permission set for the session.
    pub permissions: Arc<PermissionSet>,
    decision: Decision,
    body: Option<Bytes>,
    cancel: CancelHandle,
    /// Keeps the default channel open so an unbound context is never "cancelled".
    keepalive: Option<CancelSender>,
}
impl CallContext {
    /// Build a context with no body and a cancellation channel that never fires on its own.
    pub fn new(session: Arc<Session>, permissions: Arc<PermissionSet>) -> Self {
        let (sender, cancel) = CancelHandle::channel();
        Self {
            session,
            permissions,
            decision: Decision::Allow,
            body: None,
            cancel,
            keepalive: Some(sender),
        }
    }
    /// What the user gave for the right this command was checked against: `Substitute` tells the
    /// handler to give a stand-in that the application cannot tell from the real thing, with no error
    /// code of its own. `Allow` for a command without a right.
    pub fn decision(&self) -> Decision {
        self.decision
    }
    /// Sets the decision (builder style).
    pub fn with_decision(mut self, decision: Decision) -> Self {
        self.decision = decision;
        self
    }
    /// Session-scoped runtime path grants.
    pub fn grants(&self) -> Arc<Grants> {
        self.session.grants()
    }
    /// The session's stream hub.
    pub fn streams(&self) -> &StreamHub {
        self.session.streams()
    }
    /// The session's resource table.
    pub fn resources(&self) -> &ResourceTable {
        self.session.resources()
    }
    /// Attach a binary body (builder style).
    pub fn with_body(mut self, body: Option<Bytes>) -> Self {
        self.body = body;
        self
    }
    /// Attach the caller's cancellation handle (builder style); its sender dropping also cancels.
    pub fn with_cancel(mut self, cancel: CancelHandle) -> Self {
        self.cancel = cancel;
        self.keepalive = None;
        self
    }
    /// The binary body, if any.
    pub fn body(&self) -> Option<&Bytes> {
        self.body.as_ref()
    }
    /// The cancellation handle.
    pub fn cancel(&self) -> &CancelHandle {
        &self.cancel
    }
}
