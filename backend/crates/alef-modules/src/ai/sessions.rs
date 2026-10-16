// SPDX-License-Identifier: MIT OR Apache-2.0
//! Bounded conversation history for one AI role.
use super::{launch::start_chat, provider, relay, ChatArgs, Delivery};
use alef_core::{
    ids::ResourceId,
    registry::{command::Reply, dispatch::Registry},
    security::{consent::Decision, permissions::Permission},
    AlefError, ErrorCode,
};
use serde::Deserialize;
use serde_json::{json, Value};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenArgs {
    role: String,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct SendArgs {
    conversation: u64,
    messages: Vec<ChatMessage>,
    #[serde(default)]
    tools: Vec<String>,
    #[serde(default)]
    builtin: Vec<String>,
}
#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct CloseArgs {
    conversation: u64,
}

pub(super) fn register(
    registry: &mut Registry,
    provider: Arc<dyn provider::AiProvider>,
    host: Arc<dyn alef_core::registry::host::Host>,
    app_key: String,
) -> Result<(), AlefError> {
    registry.command::<OpenArgs>("ai.session.open")?.handler({
        let provider = provider.clone();
        move |ctx, args| {
            let provider = provider.clone();
            async move {
                match ctx
                    .permissions
                    .check(Permission::AiRole, Some(&args.role), &ctx.grants())
                {
                    Ok(Decision::Allow) => {}
                    Ok(Decision::Substitute) => {
                        return Err(AlefError::new(
                            ErrorCode::NotAvailable,
                            "the role is not available",
                        ));
                    }
                    _ => {
                        return Err(AlefError::new(
                            ErrorCode::PermissionDenied,
                            "the role is not allowed",
                        ));
                    }
                }
                if !provider.roles()?.iter().any(|r| r.id == args.role) {
                    return Err(AlefError::new(
                        ErrorCode::NotAvailable,
                        "the role is not available",
                    ));
                }
                let id = ctx
                    .resources()
                    .insert(Box::new(Conversation::new(args.role)))?;
                Ok(Reply::Json(json!({"conversation":id.0})))
            }
        }
    })?;
    registry
        .command::<SendArgs>("ai.session.send")?
        .waits()
        .handler({
            let provider = provider.clone();
            let host = host.clone();
            move |ctx, args| {
                let provider = provider.clone();
                let host = host.clone();
                let app_key = app_key.clone();
                async move {
                    let id = ResourceId(args.conversation);
                    let conversation = ctx
                        .resources()
                        .with_as::<Conversation, _>(id, Clone::clone)
                        .map_err(|_| AlefError::new(ErrorCode::Closed, "conversation is closed"))?;
                    if ctx.permissions.check(
                        Permission::AiRole,
                        Some(conversation.role()),
                        &ctx.grants(),
                    ) != Ok(Decision::Allow)
                    {
                        return Err(AlefError::new(
                            ErrorCode::PermissionDenied,
                            "the role is not allowed",
                        ));
                    }
                    let turn = conversation.begin(args.messages)?;
                    let chat = ChatArgs {
                        role: conversation.role().to_owned(),
                        messages: turn.messages().to_vec(),
                        tools: args.tools,
                        builtin: args.builtin,
                    };
                    start_chat(ctx, chat, provider, host, app_key, Some(turn)).await
                }
            }
        })?;
    registry
        .command::<CloseArgs>("ai.session.close")?
        .handler(|ctx, args| async move {
            let id = ResourceId(args.conversation);
            if let Ok(resource) = ctx.resources().with_as::<Conversation, _>(id, |_| ()) {
                let _ = resource;
                if let Ok(resource) = ctx.resources().take(id) {
                    resource.close().await;
                }
            }
            Ok(Reply::Json(Value::Null))
        })?;
    Ok(())
}

use std::{
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex, MutexGuard},
};

use super::dto::ChatMessage;
use alef_core::registry::context::CancelSender;
use alef_core::session::resources::Resource;

pub(super) async fn finish_turn(
    writer: &alef_core::session::streams::StreamWriter,
    turn: Option<Turn>,
    assistant: String,
    at: tokio::time::Instant,
) -> Result<(), AlefError> {
    if let Some(turn) = &turn {
        turn.validate_commit(&assistant)?;
    }
    match relay(writer, json!({"type": "end", "reason": "completed"}), at).await {
        Delivery::Sent => {
            if let Some(turn) = turn {
                turn.commit(assistant)?;
            }
            Ok(())
        }
        Delivery::Closed => Err(error(ErrorCode::Closed, "the chat is closed")),
        Delivery::Slow => Err(error(
            ErrorCode::Timeout,
            "the chat could not keep up with the page",
        )),
    }
}

const MAX_BYTES: usize = 256 * 1024;
const MAX_MESSAGES: usize = 32;

#[derive(Default)]
struct State {
    history: Vec<ChatMessage>,
    active: bool,
    closed: bool,
    cancel: Option<CancelSender>,
}

/// A conversation's role and shared, bounded turn state.
#[derive(Clone)]
pub(super) struct Conversation {
    role: String,
    state: Arc<Mutex<State>>,
}

impl Conversation {
    pub(super) fn new(role: String) -> Self {
        Self {
            role,
            state: Arc::new(Mutex::new(State::default())),
        }
    }

    fn state(&self) -> MutexGuard<'_, State> {
        self.state.lock().unwrap_or_else(|e| e.into_inner())
    }

    pub(super) fn role(&self) -> &str {
        &self.role
    }

    pub(super) fn begin(&self, messages: Vec<ChatMessage>) -> Result<Turn, AlefError> {
        let mut state = self.state();
        if state.closed {
            return Err(error(ErrorCode::Closed, "conversation is closed"));
        }
        if state.active {
            return Err(error(
                ErrorCode::Busy,
                "conversation already has an active turn",
            ));
        }
        let mut combined = state.history.clone();
        combined.extend(messages);
        check_bounds(&combined)?;
        state.active = true;
        Ok(Turn {
            state: self.state.clone(),
            messages: combined,
        })
    }
}

impl Resource for Conversation {
    fn close(self: Box<Self>) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        {
            let mut state = self.state();
            state.closed = true;
            state.history.clear();
            if let Some(cancel) = state.cancel.take() {
                cancel.cancel();
            }
        }
        Box::pin(async {})
    }
}

/// A reserved turn. Dropping it releases the reservation without changing history.
pub(super) struct Turn {
    state: Arc<Mutex<State>>,
    messages: Vec<ChatMessage>,
}

impl std::fmt::Debug for Turn {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("Turn")
            .field("messages", &self.messages)
            .finish_non_exhaustive()
    }
}

impl Turn {
    pub(super) fn messages(&self) -> &[ChatMessage] {
        &self.messages
    }

    pub(super) fn attach_cancel(&self, cancel: CancelSender) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.closed {
            cancel.cancel();
        } else {
            state.cancel = Some(cancel);
        }
    }

    pub(super) fn closed(&self) -> bool {
        self.state.lock().unwrap_or_else(|e| e.into_inner()).closed
    }

    pub(super) fn validate_commit(&self, assistant: &str) -> Result<(), AlefError> {
        let mut stored = self.messages.clone();
        stored.push(ChatMessage::Assistant {
            content: assistant.to_owned(),
        });
        check_bounds(&stored)
    }

    pub(super) fn commit(self, assistant: String) -> Result<(), AlefError> {
        self.validate_commit(&assistant)?;
        let mut stored = self.messages.clone();
        stored.push(ChatMessage::Assistant { content: assistant });
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if state.closed {
            return Err(error(ErrorCode::Closed, "conversation is closed"));
        }
        state.history = stored;
        state.cancel = None;
        Ok(())
    }
}

impl Drop for Turn {
    fn drop(&mut self) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        if let Some(cancel) = state.cancel.take() {
            cancel.cancel();
        }
        state.active = false;
    }
}

fn check_bounds(messages: &[ChatMessage]) -> Result<(), AlefError> {
    if messages.len() > MAX_MESSAGES {
        return Err(error(
            ErrorCode::InvalidArgument,
            "conversation exceeds 32 messages",
        ));
    }
    let bytes = serde_json::to_vec(messages).map_err(|_| {
        error(
            ErrorCode::InvalidArgument,
            "conversation cannot be serialized",
        )
    })?;
    if bytes.len() > MAX_BYTES {
        return Err(error(
            ErrorCode::InvalidArgument,
            "conversation exceeds 256 KiB",
        ));
    }
    Ok(())
}

fn error(code: ErrorCode, message: &str) -> AlefError {
    AlefError::new(code, message.to_owned())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn user(text: &str) -> ChatMessage {
        ChatMessage::User {
            content: text.to_owned(),
        }
    }

    #[tokio::test]
    async fn closed_stream_does_not_commit_completed_turn() {
        use alef_core::session::streams::StreamHub;
        let hub = StreamHub::new(1024, 256);
        let (writer, id) = hub.open_outgoing();
        drop(hub.reader(id).unwrap());
        let conversation = Conversation::new("helper".into());
        let turn = conversation.begin(vec![user("not delivered")]).unwrap();
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            finish_turn(
                &writer,
                Some(turn),
                "answer".into(),
                tokio::time::Instant::now() + std::time::Duration::from_secs(5),
            ),
        )
        .await
        .expect("finish_turn timed out");
        assert_eq!(result.unwrap_err().code, ErrorCode::Closed);
        assert_eq!(conversation.state().history.len(), 0);
        assert_eq!(
            conversation.begin(vec![user("fresh")]).unwrap().messages(),
            &[user("fresh")]
        );
    }

    #[tokio::test]
    async fn delivered_end_commits_turn() {
        use alef_core::session::streams::StreamHub;
        let hub = StreamHub::new(1024, 256);
        let (writer, id) = hub.open_outgoing();
        let mut reader = hub.reader(id).unwrap();
        let conversation = Conversation::new("helper".into());
        let turn = conversation.begin(vec![user("delivered")]).unwrap();
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            finish_turn(
                &writer,
                Some(turn),
                "answer".into(),
                tokio::time::Instant::now() + std::time::Duration::from_secs(5),
            ),
        )
        .await
        .expect("finish_turn timed out")
        .unwrap();
        assert_eq!(
            tokio::time::timeout(std::time::Duration::from_secs(5), reader.next_frame())
                .await
                .expect("waiting for end frame timed out"),
            Some(alef_core::protocol::frame::Frame::Json(json!({
                "type": "end",
                "reason": "completed"
            })))
        );
        assert_eq!(
            conversation.state().history,
            vec![
                user("delivered"),
                ChatMessage::Assistant {
                    content: "answer".into()
                }
            ]
        );
        assert_eq!(
            conversation.begin(vec![user("fresh")]).unwrap().messages(),
            &[
                user("delivered"),
                ChatMessage::Assistant {
                    content: "answer".into()
                },
                user("fresh")
            ]
        );
    }

    #[test]
    fn a_session_begin_rejects_a_concurrent_turn() {
        let conversation = Conversation::new("helper".into());
        let turn = conversation.begin(vec![user("hi")]).unwrap();
        assert_eq!(
            conversation.begin(vec![user("again")]).unwrap_err().code,
            ErrorCode::Busy
        );
        drop(turn);
    }

    #[test]
    fn a_session_commit_keeps_both_message_roles() {
        let conversation = Conversation::new("helper".into());
        conversation
            .begin(vec![user("hi")])
            .unwrap()
            .commit("hello".into())
            .unwrap();
        let turn = conversation.begin(vec![user("next")]).unwrap();
        assert_eq!(
            turn.messages(),
            &[
                user("hi"),
                ChatMessage::Assistant {
                    content: "hello".into()
                },
                user("next")
            ]
        );
    }

    #[test]
    fn a_session_drop_rolls_back_an_uncommitted_turn() {
        let conversation = Conversation::new("helper".into());
        drop(conversation.begin(vec![user("uncommitted")]).unwrap());
        assert_eq!(
            conversation.begin(vec![user("fresh")]).unwrap().messages(),
            &[user("fresh")]
        );
    }

    #[test]
    fn a_session_close_clears_history_and_rejects_future_turns() {
        let conversation = Conversation::new("helper".into());
        conversation
            .begin(vec![user("hi")])
            .unwrap()
            .commit("hello".into())
            .unwrap();
        let resource: Box<dyn Resource> = Box::new(conversation.clone());
        drop(resource.close());
        assert_eq!(
            conversation.begin(vec![user("again")]).unwrap_err().code,
            ErrorCode::Closed
        );
        assert!(conversation.state().history.is_empty());
    }

    #[test]
    fn closing_a_session_cancels_an_active_turn_and_prevents_history_resurrection() {
        let conversation = Conversation::new("helper".into());
        let turn = conversation.begin(vec![user("active")]).unwrap();
        let (sender, handle) = alef_core::registry::context::CancelHandle::channel();
        turn.attach_cancel(sender);
        let resource: Box<dyn Resource> = Box::new(conversation.clone());
        drop(resource.close());
        assert!(handle.is_cancelled());
        assert!(turn.closed());
        assert_eq!(
            turn.commit("late".into()).unwrap_err().code,
            ErrorCode::Closed
        );
        assert!(conversation.state().history.is_empty());
    }

    #[test]
    fn an_oversized_assistant_response_does_not_commit_the_failed_turn() {
        let conversation = Conversation::new("helper".into());
        let turn = conversation.begin(vec![user("failed")]).unwrap();
        assert_eq!(
            turn.commit("x".repeat(MAX_BYTES)).unwrap_err().code,
            ErrorCode::InvalidArgument
        );
        assert_eq!(
            conversation.begin(vec![user("fresh")]).unwrap().messages(),
            &[user("fresh")]
        );
    }

    #[tokio::test]
    async fn unloading_the_document_drops_history_and_cancels_its_active_turn() {
        use alef_core::session::resources::ResourceTable;
        use std::time::Duration;

        let conversation = Conversation::new("helper".into());
        conversation
            .begin(vec![user("initial")])
            .unwrap()
            .commit("history".into())
            .unwrap();
        let turn = conversation.begin(vec![user("active")]).unwrap();
        let (sender, handle) = alef_core::registry::context::CancelHandle::channel();
        turn.attach_cancel(sender);
        let table = ResourceTable::new(2);
        table.insert(Box::new(conversation.clone())).unwrap();

        tokio::time::timeout(Duration::from_secs(5), table.close_all())
            .await
            .unwrap();
        assert!(handle.is_cancelled());
        assert!(conversation.state().history.is_empty());
        assert_eq!(
            turn.commit("late".into()).unwrap_err().code,
            ErrorCode::Closed
        );
        assert_eq!(
            conversation.begin(vec![user("fresh")]).unwrap_err().code,
            ErrorCode::Closed
        );
    }

    #[test]
    fn a_session_rejects_message_and_byte_bounds() {
        let conversation = Conversation::new("helper".into());
        assert_eq!(
            conversation
                .begin(vec![user("x"); MAX_MESSAGES + 1])
                .unwrap_err()
                .code,
            ErrorCode::InvalidArgument
        );
        assert_eq!(
            conversation
                .begin(vec![user(&"x".repeat(MAX_BYTES))])
                .unwrap_err()
                .code,
            ErrorCode::InvalidArgument
        );
    }
}
