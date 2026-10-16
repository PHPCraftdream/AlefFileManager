// SPDX-License-Identifier: MIT OR Apache-2.0
//! The provider interface of the `ai` module and its two implementations: none (a build without a
//! bridge to Rush) and a deterministic fake (tests and end-to-end runs). A real one arrives with
//! the Rush transport; everything behind the trait is untrusted from here on.
use std::sync::atomic::{AtomicUsize, Ordering};

use alef_core::{registry::context::CancelHandle, AlefError, ErrorCode};
use serde::Serialize;
use serde_json::{json, Value};
use tokio::sync::{mpsc, oneshot};

use super::dto::{ChatMessage, Role};

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
#[derive(Debug, Clone, PartialEq)]
pub struct ToolDef {
    pub name: String,
    pub description: String,
    pub schema: Value,
}

/// What one chat asks of the provider.
#[derive(Debug, Clone)]
pub struct ChatRequest {
    pub role: String,
    pub messages: Vec<ChatMessage>,
    pub tools: Vec<ToolDef>,
    pub builtin: Vec<ToolDef>,
    pub history: bool,
}

/// How the module drives a chat it has already handed to the provider: cancellation, and the
/// results of the tool calls the model made.
pub struct ChatControl {
    pub cancel: CancelHandle,
    pub results: mpsc::Receiver<Result<Value, AlefError>>,
}

/// Where the chats really go. Every method returns at once; the streaming ones hand out handles.
pub trait AiProvider: Send + Sync + std::fmt::Debug {
    /// The roles this build can serve; the module publishes only the allowed ones of these.
    fn roles(&self) -> Result<Vec<Role>, AlefError>;
    /// Starts a chat; the events come on the receiver, the results on `control`.
    fn chat(
        &self,
        request: ChatRequest,
        control: ChatControl,
    ) -> Result<mpsc::Receiver<ChatEvent>, AlefError>;
}

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

/// The pause between the events of the fake, so that cancellation is observable mid-stream.
const PACE: std::time::Duration = std::time::Duration::from_millis(10);
/// How long the fake waits for one tool result before it gives the chat up.
const RESULT_WAIT: std::time::Duration = std::time::Duration::from_secs(30);

/// A deterministic provider for tests and end-to-end runs: it thinks, calls the tools the request
/// listed (in order, with fixed arguments), waits for one result per call, reports what it saw
/// (only when the request listed tools: a no-tool chat answers without a Saw text), accounts a
/// round-based usage and completes. It knows nothing about registrations; the module
/// decides which tools reach it.
#[derive(Debug, Default)]
pub struct FakeProvider {
    started: AtomicUsize,
    next_chat_gate: std::sync::Mutex<Option<oneshot::Receiver<()>>>,
}

/// Opens a single fake chat's producer gate. Dropping the guard also releases it.
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
}

impl AiProvider for FakeProvider {
    fn roles(&self) -> Result<Vec<Role>, AlefError> {
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
        self.started.fetch_add(1, Ordering::SeqCst);
        let (sender, receiver) = mpsc::channel(16);
        let gate = self
            .next_chat_gate
            .lock()
            .expect("fake chat gate lock")
            .take();
        tokio::spawn(produce(request, control, sender, gate));
        Ok(receiver)
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
) {
    if let Some(gate) = gate {
        tokio::select! {
            _ = control.cancel.cancelled() => return,
            _ = gate => {}
        }
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
    use super::*;
    use crate::ai::dto::ChatMessage;
    use alef_core::registry::context::CancelHandle;

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
}
