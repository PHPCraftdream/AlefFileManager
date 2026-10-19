// SPDX-License-Identifier: MIT OR Apache-2.0
//! The provider interface and child-process bridge plumbing.
use std::{
    future::Future,
    pin::Pin,
    sync::atomic::{AtomicUsize, Ordering},
};

use alef_core::{registry::context::CancelHandle, AlefError, ErrorCode};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::{mpsc, oneshot};

#[path = "rush/process.rs"]
pub(crate) mod rush_process;

/// A Rush role the application may ask for (`ai.roles`), as the provider describes it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase", deny_unknown_fields)]
#[ts(export, export_to = "modules.ts")]
pub struct Role {
    pub id: String,
    pub title: String,
    pub description: String,
    /// What the role can do (`text`, `tool_calls`, `usage`, `cancel`).
    pub capabilities: Vec<String>,
}

/// One message of the history the application sends; only `user` and `assistant` are allowed.
/// (A tagged enum buffers its content, so `deny_unknown_fields` says nothing here.)
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(tag = "role", rename_all = "lowercase")]
#[ts(export, export_to = "modules.ts")]
pub enum ChatMessage {
    User { content: String },
    Assistant { content: String },
}

/// One part of a chat as it streams to the page (`ai.chat` iterator).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(tag = "type", rename_all = "snake_case")]
#[ts(export, export_to = "modules.ts")]
pub enum ChatPart {
    Text {
        text: String,
    },
    ToolCall {
        id: String,
        name: String,
        #[ts(type = "unknown")]
        arguments: serde_json::Value,
    },
    #[serde(rename_all = "camelCase")]
    Usage {
        input_tokens: Option<u64>,
        output_tokens: Option<u64>,
        cache_read_tokens: Option<u64>,
        cache_creation_tokens: Option<u64>,
        estimated: bool,
        complete: bool,
    },
    End {
        #[serde(default = "completed")]
        reason: String,
    },
}

fn completed() -> String {
    "completed".to_owned() // the stub backend's only end reason
}

/// What the page asks of `ai.chat`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(export, export_to = "modules.ts")]
pub struct ChatOptions {
    pub role: String,
    pub messages: Vec<ChatMessage>,
    #[serde(default)]
    pub tools: Vec<String>,
    #[serde(default)]
    pub builtin: Vec<String>,
}

/// One event of a chat, as the provider produces it and the module relays to the page.
#[derive(Debug, Clone, PartialEq)]
pub enum ChatEvent {
    Text(String),
    ToolCall {
        call_id: String,
        name: String,
        arguments: Value,
    },
    Usage(ChatUsage),
    Completed,
    /// A coded failure of the provider itself; the module relays it as the chat error.
    Failed {
        code: ErrorCode,
        message: String,
    },
}

/// Cumulative token use of the chat so far; snapshots, never additions of snapshots.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ChatUsage {
    pub input_tokens: Option<u64>,
    pub output_tokens: Option<u64>,
    pub cache_read_tokens: Option<u64>,
    pub cache_creation_tokens: Option<u64>,
    pub estimated: bool,
    pub complete: bool,
}

/// A tool the model may call: what it is called, what it is told it does, and the shape of its
/// arguments.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub schema: Value,
}

/// What one chat asks of the provider.
#[derive(Debug, Clone, Serialize)]
pub struct ChatRequest {
    pub role: String,
    pub messages: Vec<ChatMessage>,
    pub tools: Vec<ToolDef>,
    pub builtin: Vec<ToolDef>,
    pub history: bool,
    /// The Alef conversation this turn belongs to; `None` for a stateless chat.
    pub conversation: Option<String>,
}

/// How the module drives a running chat: cancellation and the tool-call results.
pub struct ChatControl {
    pub cancel: CancelHandle,
    pub results: mpsc::Receiver<Result<Value, AlefError>>,
}

/// Where the chats really go. Every method returns at once; the streaming ones hand out handles.
pub trait AiProvider: Send + Sync + std::fmt::Debug {
    /// The roles this build can serve; the module publishes only the allowed ones of these.
    fn roles(&self) -> Result<Vec<Role>, AlefError>;
    /// Async form of `roles` for handlers; the default just boxes the sync one.
    fn roles_fut(&self) -> Pin<Box<dyn Future<Output = Result<Vec<Role>, AlefError>> + Send + '_>> {
        Box::pin(async { self.roles() })
    }
    /// Starts a chat; streaming hands out handles; results arrive on `control`.
    fn chat(
        &self,
        request: ChatRequest,
        control: ChatControl,
    ) -> Result<mpsc::Receiver<ChatEvent>, AlefError>;
    /// The tokens this chat will at most use, when the provider can name a bound; `None` is unknown, and a user's token limit together with `None` refuses the attempt
    /// (docs/stages/m9-ai.md §6).
    fn token_bound(&self, _request: &ChatRequest) -> Option<u64> {
        None
    }
    /// Asks the provider to drop any stored conversation state behind this request
    /// (best effort; the fake and the missing provider keep nothing).
    fn forget(&self, _request: &ChatRequest) {}
}

#[cfg(test)]
pub(crate) use rush_process::BoundedReader;
pub use rush_process::RushCommand;
#[cfg(test)]
pub(crate) use rush_process::{effective_of, filter_env};
pub(crate) use rush_process::{
    fail_after, run_probe, spawn_child, spawnable_program, sync, ProbeOutcome, SessionMap,
    ALLOWED_PROVIDERS,
};

fn missing() -> AlefError {
    AlefError::new(ErrorCode::NotAvailable, "no ai provider in this build")
}

/// The stand-in of a build without a bridge to Rush: every operation says so.
#[derive(Debug, Default)]
pub struct MissingProvider;

impl AiProvider for MissingProvider {
    fn roles(&self) -> Result<Vec<Role>, AlefError> {
        Err(missing())
    }
    fn chat(
        &self,
        _request: ChatRequest,
        _control: ChatControl,
    ) -> Result<mpsc::Receiver<ChatEvent>, AlefError> {
        Err(missing())
    }
}

/// Pause between fake events so cancellation is observable mid-stream.
const PACE: std::time::Duration = std::time::Duration::from_millis(10);
/// How long the fake waits for one tool result.
const RESULT_WAIT: std::time::Duration = std::time::Duration::from_secs(30);
/// The token upper bound the fake names for every chat.
const FAKE_TOKEN_BOUND: u64 = 4096;
const SEEN_CAP: usize = 256;

/// A deterministic provider for tests and e2e runs: fixed tool calls, one result wait, what it
/// saw (with tools), round-based usage, one completion; registrations are the module's call.
#[derive(Debug, Default)]
pub struct FakeProvider {
    started: AtomicUsize,
    next_chat_gate: std::sync::Mutex<Option<oneshot::Receiver<()>>>,
    /// Test seam: the failure `roles` answers with until cleared.
    roles_failure: std::sync::Mutex<Option<String>>,
    /// Test seam: the failure the next `chat` start answers with until cleared.
    chat_failure: std::sync::Mutex<Option<String>>,
    /// Test seam: the next chat emits a coded provider failure after starting.
    async_failure: std::sync::Mutex<Option<(ErrorCode, String)>>,
    seen: std::sync::Mutex<Vec<ChatRequest>>,
}

/// Opens one fake chat's producer gate; dropping the guard releases it.
#[derive(Debug)]
pub struct ChatGate(Option<oneshot::Sender<()>>);

impl ChatGate {
    pub fn release(mut self) {
        if let Some(sender) = self.0.take() {
            let _ = sender.send(());
        }
    }
}

impl Drop for ChatGate {
    fn drop(&mut self) {
        if let Some(sender) = self.0.take() {
            let _ = sender.send(());
        }
    }
}

impl FakeProvider {
    /// Hold the next chat before its first event; dropping the returned guard releases it.
    pub fn hold_next_chat(&self) -> ChatGate {
        let (sender, receiver) = oneshot::channel();
        *self.next_chat_gate.lock().expect("fake chat gate lock") = Some(receiver);
        ChatGate(Some(sender))
    }

    /// How many chats this provider has started.
    pub fn chats_started(&self) -> usize {
        self.started.load(Ordering::SeqCst)
    }

    /// Seeds the failure `roles` answers with; `None` clears it (a stand-in for raw provider
    /// text — credentials included — echoed into the details).
    pub fn seed_roles_failure(&self, message: Option<String>) {
        *self.roles_failure.lock().expect("fake roles failure lock") = message;
    }

    /// Seeds the failure the next `chat` start answers with; `None` clears it (see above).
    pub fn seed_chat_failure(&self, message: Option<String>) {
        *self.chat_failure.lock().expect("fake chat failure lock") = message;
    }

    /// Seeds a coded asynchronous failure for the next chat event.
    pub fn seed_async_failure(&self, failure: Option<(ErrorCode, String)>) {
        *self.async_failure.lock().expect("fake async failure lock") = failure;
    }

    /// The most recent requests this provider received, oldest first: the boundary observer of
    /// the tests. The log is capped so a long test cannot grow it without bound.
    pub fn requests(&self) -> Vec<ChatRequest> {
        self.seen.lock().expect("fake request log lock").clone()
    }
}

impl AiProvider for FakeProvider {
    fn roles(&self) -> Result<Vec<Role>, AlefError> {
        if let Some(message) = self
            .roles_failure
            .lock()
            .expect("fake roles failure lock")
            .clone()
        {
            return Err(AlefError::new(ErrorCode::Internal, message.clone())
                .with_details(serde_json::json!({ "provider": message })));
        }
        Ok(vec![
            Role {
                id: "smart".to_owned(),
                title: "Smart".to_owned(),
                description: "the careful, slower role".to_owned(),
                capabilities: vec![
                    "text".to_owned(),
                    "tool_calls".to_owned(),
                    "usage".to_owned(),
                    "cancel".to_owned(),
                ],
            },
            Role {
                id: "fast".to_owned(),
                title: "Fast".to_owned(),
                description: "the quick, lighter role".to_owned(),
                capabilities: vec!["text".to_owned(), "usage".to_owned()],
            },
        ])
    }
    fn chat(
        &self,
        request: ChatRequest,
        control: ChatControl,
    ) -> Result<mpsc::Receiver<ChatEvent>, AlefError> {
        if let Some(message) = self
            .chat_failure
            .lock()
            .expect("fake chat failure lock")
            .clone()
        {
            return Err(AlefError::new(ErrorCode::Internal, message.clone())
                .with_details(serde_json::json!({ "provider": message })));
        }
        self.started.fetch_add(1, Ordering::SeqCst);
        {
            let mut seen = self.seen.lock().expect("fake request log lock");
            seen.push(request.clone());
            if seen.len() > SEEN_CAP {
                let excess = seen.len() - SEEN_CAP;
                seen.drain(0..excess);
            }
        }
        let (sender, receiver) = mpsc::channel(16);
        let gate = self
            .next_chat_gate
            .lock()
            .expect("fake chat gate lock")
            .take();
        let failure = self
            .async_failure
            .lock()
            .expect("fake async failure lock")
            .take();
        tokio::spawn(produce(request, control, sender, gate, failure));
        Ok(receiver)
    }
    fn token_bound(&self, _request: &ChatRequest) -> Option<u64> {
        Some(FAKE_TOKEN_BOUND)
    }
}

/// Sends one event unless the chat was cancelled or the module stopped listening.
async fn emit(control: &ChatControl, sender: &mpsc::Sender<ChatEvent>, event: ChatEvent) -> bool {
    tokio::time::sleep(PACE).await;
    tokio::select! {
        _ = control.cancel.cancelled() => false,
        sent = sender.send(event) => sent.is_ok(),
    }
}

/// What the fake reports having seen: the summary field when there is one, the compact JSON cut
/// to 64 characters otherwise.
fn echo(value: &Value) -> String {
    match value.get("summary").and_then(|seen| seen.as_str()) {
        Some(summary) => summary.to_owned(),
        None => serde_json::to_string(value)
            .unwrap_or_default()
            .chars()
            .take(64)
            .collect(),
    }
}

async fn produce(
    request: ChatRequest,
    mut control: ChatControl,
    sender: mpsc::Sender<ChatEvent>,
    gate: Option<oneshot::Receiver<()>>,
    failure: Option<(ErrorCode, String)>,
) {
    if let Some(gate) = gate {
        tokio::select! {
            _ = control.cancel.cancelled() => return,
            _ = gate => {}
        }
    }
    if let Some((code, message)) = failure {
        let _ = emit(&control, &sender, ChatEvent::Failed { code, message }).await;
        return;
    }
    if request.history {
        let history = serde_json::to_string(&request.messages).unwrap_or_default();
        if !emit(
            &control,
            &sender,
            ChatEvent::Text(format!("History {history}")),
        )
        .await
        {
            return;
        }
    } else if !emit(&control, &sender, ChatEvent::Text("Thinking".to_owned())).await {
        return;
    }
    let arguments = request
        .messages
        .iter()
        .rev()
        .find_map(|message| match message {
            ChatMessage::User { content } => serde_json::from_str::<Value>(content).ok(),
            _ => None,
        })
        .unwrap_or_else(|| json!({"input":"ping"}));
    let calls = request
        .tools
        .iter()
        .chain(request.builtin.iter())
        .enumerate()
        .map(|(index, tool)| ChatEvent::ToolCall {
            call_id: format!("call-{index}"),
            name: tool.name.clone(),
            arguments: arguments.clone(),
        })
        .collect::<Vec<_>>();
    for call in calls {
        let ChatEvent::ToolCall {
            call_id,
            name,
            arguments,
        } = call
        else {
            unreachable!("the calls were built just above");
        };
        if !emit(
            &control,
            &sender,
            ChatEvent::ToolCall {
                call_id: call_id.clone(),
                name: name.clone(),
                arguments: arguments.clone(),
            },
        )
        .await
        {
            return;
        }
    }
    let mut last = String::new();
    let mut results = 0_usize;
    for _ in 0..request.tools.len() + request.builtin.len() {
        let wait = tokio::time::timeout(RESULT_WAIT, control.results.recv());
        match wait.await {
            Ok(Some(Ok(value))) => {
                results += 1;
                last = echo(&value);
            }
            // The module went away, or one handler took too long: the chat fails.
            _ => return,
        }
    }
    // Only a chat with tools reports what it saw; a no-tool chat answers the request directly.
    if (!request.tools.is_empty() || !request.builtin.is_empty())
        && !emit(
            &control,
            &sender,
            ChatEvent::Text(format!("Saw {results} tool result(s): {last}")),
        )
        .await
    {
        return;
    }
    let rounds = (request.tools.len() + request.builtin.len()) as u64;
    if !emit(
        &control,
        &sender,
        ChatEvent::Usage(ChatUsage {
            input_tokens: Some(10 + 5 * rounds),
            output_tokens: Some(5 + 3 * results as u64),
            cache_read_tokens: None,
            cache_creation_tokens: None,
            estimated: true,
            complete: true,
        }),
    )
    .await
    {
        return;
    }
    let _ = sender.send(ChatEvent::Completed).await;
    // Dropping `sender` here ends the chat exactly once; nothing follows the completion.
}

#[cfg(test)]
mod tests {
    use super::rush_process::MAX_SESSIONS;
    use super::*;
    use alef_core::registry::context::CancelHandle;
    use std::path::Path;

    fn unavailable(result: Result<(), AlefError>) {
        let error = result.unwrap_err();
        assert_eq!(error.code, ErrorCode::NotAvailable);
        assert_eq!(error.message, "no ai provider in this build");
    }

    #[test]
    fn without_a_provider_every_operation_says_it_is_not_available() {
        let provider = MissingProvider;
        unavailable(provider.roles().map(|_| ()));
        let (cancel, cancelled) = CancelHandle::channel();
        let (results, result_receiver) = mpsc::channel(1);
        unavailable(
            provider
                .chat(
                    ChatRequest {
                        role: "smart".to_owned(),
                        messages: vec![ChatMessage::User {
                            content: "hi".to_owned(),
                        }],
                        tools: Vec::new(),
                        builtin: Vec::new(),
                        history: false,
                        conversation: None,
                    },
                    ChatControl {
                        cancel: cancelled,
                        results: result_receiver,
                    },
                )
                .map(|_| ()),
        );
        drop(cancel);
        drop(results);
    }

    #[test]
    fn session_map_evicts_oldest_and_returns_all_live_ids_for_teardown() {
        let mut sessions = SessionMap::default();
        for index in 0..MAX_SESSIONS {
            assert_eq!(
                sessions.insert(format!("conv-{index}"), format!("alef-{index}")),
                None
            );
        }
        assert_eq!(sessions.len(), MAX_SESSIONS);
        assert_eq!(
            sessions.insert("next".into(), "alef-next".into()),
            Some("alef-0".into())
        );
        let all = sessions.ids(usize::MAX);
        assert_eq!(all.len(), MAX_SESSIONS);
        assert!(!all.contains(&"alef-0".to_owned()));
        assert!(all.contains(&"alef-next".to_owned()));
    }

    #[test]
    fn only_spawnable_programs_are_accepted() {
        assert!(spawnable_program(Path::new("rush.exe")));
        assert!(spawnable_program(Path::new("RUSH.COM")));
        #[cfg(windows)]
        {
            assert!(!spawnable_program(Path::new("rush.cmd")));
            assert!(!spawnable_program(Path::new("rush.bat")));
            assert!(!spawnable_program(Path::new("rush")));
        }
        #[cfg(not(windows))]
        assert!(spawnable_program(Path::new("rush")));
    }
}
