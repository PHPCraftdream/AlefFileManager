// SPDX-License-Identifier: MIT OR Apache-2.0
//! The `ai` module: the bridge from the document to the Rush roles the manifest declares
//! (docs/stages/m9-ai.md §2, §5, §6), behind a provider stub. A role is asked for with `ai.chat`,
//! which streams text, tool calls and usage to the page over an outgoing stream; the page answers
//! the tool calls of the application's own tools with `ai.tools.result`. Everything is scoped by
//! `permissions.ai` and the consent of the user: a substituted role answers `NOT_AVAILABLE`
//! without reaching the provider, a denied one is `PERMISSION_DENIED`, and a substituted tool is
//! registered inert and simply absent from the chat.
use std::{
    collections::{HashMap, HashSet},
    future::Future,
    pin::Pin,
    sync::{Arc, Mutex, MutexGuard},
    time::{Duration, Instant},
};

use alef_core::{
    ids::ResourceId,
    registry::{
        command::Reply,
        context::{CallContext, CancelHandle, CancelSender},
        dispatch::Registry,
        host::Host,
    },
    security::{consent::Decision, permissions::Permission},
    session::{resources::Resource, session::Session, streams::StreamWriter},
    AlefError, ErrorCode,
};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::mpsc;

use crate::{json, ModuleContext};
use dto::{ChatMessage, Role};
use provider::{AiProvider, ChatControl, ChatEvent, ChatRequest, ToolDef};

pub mod dto;
pub mod limits;
pub mod provider;

/// The live tool registrations of every session: `(session, tool name)` to the registration
/// resource id and whether it is inert (a substituted tool: registered, never offered). A short
/// lock, never held across an await.
static TOOLS: std::sync::LazyLock<Mutex<HashMap<(u64, String), (u64, bool)>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

fn tools() -> MutexGuard<'static, HashMap<(u64, String), (u64, bool)>> {
    TOOLS.lock().unwrap_or_else(|error| error.into_inner())
}

/// The live chats of every session: the session id to its chat ids and their state. A short lock,
/// never held across an await (docs/stages/m9-ai.md §6).
static CHATS: std::sync::LazyLock<Mutex<HashMap<u64, HashMap<u64, Arc<ChatState>>>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

fn chats() -> MutexGuard<'static, HashMap<u64, HashMap<u64, Arc<ChatState>>>> {
    CHATS.lock().unwrap_or_else(|error| error.into_inner())
}

/// Forgets one chat of one session; an empty session leaves the map too.
fn forget_chat(session: u64, chat: u64) {
    let mut all = chats();
    if let Some(own) = all.get_mut(&session) {
        own.remove(&chat);
        if own.is_empty() {
            all.remove(&session);
        }
    }
}

fn invalid(message: String) -> AlefError {
    AlefError::new(ErrorCode::InvalidArgument, message)
}

fn internal(message: &str) -> AlefError {
    AlefError::new(ErrorCode::Internal, message.to_owned())
}

fn busy(message: String) -> AlefError {
    AlefError::new(ErrorCode::Busy, message)
}

/// A stale, replayed, foreign or cancelled tool result; every chat that is over answers alike.
fn closed_call() -> AlefError {
    AlefError::new(ErrorCode::Closed, "the tool call is no longer pending")
}

fn timeout_of(message: &str) -> AlefError {
    AlefError::new(ErrorCode::Timeout, message.to_owned())
}

/// One tool call of the chat the page has not answered yet; dropped unanswered when the chat dies.
struct PendingCall {
    /// When the handler has waited long enough; the producer races this.
    deadline: Instant,
    /// The tool the call belongs to; a result for another name never burns the call.
    name: String,
}

/// The state of one chat: its pending tool calls, dead once the chat is over.
struct ChatState {
    pending: Mutex<HashMap<String, PendingCall>>,
    cancelled: Mutex<bool>,
}

impl ChatState {
    fn new() -> Self {
        Self {
            pending: Mutex::new(HashMap::new()),
            cancelled: Mutex::new(false),
        }
    }
    /// Whether the chat is over; results that come later are refused before the lookup.
    fn cancelled(&self) -> bool {
        *self
            .cancelled
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }
    fn pending(&self) -> MutexGuard<'_, HashMap<String, PendingCall>> {
        self.pending
            .lock()
            .unwrap_or_else(|error| error.into_inner())
    }
    /// Ends every pending call (their senders drop) and stops new ones.
    fn fail(&self) {
        *self
            .cancelled
            .lock()
            .unwrap_or_else(|error| error.into_inner()) = true;
        self.pending().clear();
    }
}

/// Whether a pending call has waited past its deadline: strictly after, not at (docs/stages/m9-ai.md §6).
fn call_expired(deadline: Instant, now: Instant) -> bool {
    now > deadline
}

/// A tool failure the page may see: control characters dropped, at most 200 characters kept.
fn sanitize_failure(message: &str) -> String {
    message
        .chars()
        .filter(|c| !c.is_control())
        .take(200)
        .collect()
}

/// The handle of a running chat in the resource table: closing it (the session is unloaded, or the
/// page is gone) cancels the provider and deadens the pending calls.
struct ChatHandle {
    state: Arc<ChatState>,
    cancel: CancelSender,
    results: mpsc::Sender<Result<Value, AlefError>>,
}

impl Resource for ChatHandle {
    fn close(self: Box<Self>) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        self.cancel.cancel();
        self.state.fail();
        Box::pin(async {})
    }
}

/// A registered tool of the session; closing it (explicit unregister or session teardown) removes
/// it from the global map, so no later chat offers it.
struct Registration {
    key: (u64, String),
}

impl Resource for Registration {
    fn close(self: Box<Self>) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        tools().remove(&self.key);
        Box::pin(async {})
    }
}

/// Cancels the provider, deadens the pending calls and forgets the resource.
fn abandon_chat(
    state: &Arc<ChatState>,
    cancel: &CancelSender,
    session: &Session,
    resource: ResourceId,
    chat: u64,
) {
    cancel.cancel();
    state.fail();
    let _ = session.resources().remove(resource);
    forget_chat(session.id().0, chat);
}

/// Sends one frame of the chat within the idle budget: false when the page could not keep up.
async fn relay(writer: &StreamWriter, frame: Value, budget: Duration) -> bool {
    tokio::time::timeout(budget, writer.send_json(frame))
        .await
        .is_ok()
}

/// Whether a serialized frame of the chat is larger than a part may be.
fn oversized(frame: &Value) -> bool {
    serde_json::to_vec(frame).is_ok_and(|bytes| bytes.len() > limits::MAX_TEXT_PART_BYTES)
}

/// The form of a chat before anything reaches the provider (docs/stages/m9-ai.md §6).
fn validate_chat(args: &ChatArgs) -> Result<(), AlefError> {
    if args.messages.is_empty() {
        return Err(invalid("a chat needs at least one message".to_owned()));
    }
    for message in &args.messages {
        let content = match message {
            ChatMessage::User { content } => content,
            ChatMessage::Assistant { content } => content,
        };
        if content.len() > limits::MAX_CONTENT_BYTES {
            return Err(invalid("a message is longer than 64 KiB".to_owned()));
        }
    }
    let bytes = serde_json::to_vec(args)
        .map_err(|_| invalid("the arguments of the chat do not serialize".to_owned()))?;
    if bytes.len() > limits::MAX_MESSAGE_BYTES {
        return Err(invalid("the chat is longer than 256 KiB".to_owned()));
    }
    if args.tools.len() > limits::MAX_TOOLS {
        return Err(invalid("too many tools for one chat".to_owned()));
    }
    Ok(())
}

/// The tools of the chat that reach the provider: allowed, registered, and not inert. A
/// substituted tool is left out, a denied or undeclared one is refused, and a declared and allowed
/// one without a live registration is an argument error before anything starts.
fn allowed_tools(ctx: &CallContext, args: &ChatArgs) -> Result<Vec<ToolDef>, AlefError> {
    let mut seen = HashSet::new();
    let mut offered = Vec::new();
    for name in &args.tools {
        if !seen.insert(name.as_str()) {
            return Err(invalid(format!("the tool {name} is named twice")));
        }
        match ctx
            .permissions
            .check(Permission::AiTool, Some(name), &ctx.grants())
        {
            Ok(Decision::Allow) => {}
            Ok(Decision::Substitute) => continue,
            Ok(Decision::Deny) | Err(_) => {
                return Err(AlefError::new(
                    ErrorCode::PermissionDenied,
                    format!("the tool {name} is not allowed"),
                ));
            }
        }
        let registered = tools().get(&(ctx.session.id().0, name.clone())).copied();
        match registered {
            Some((_, false)) => {}
            // An inert registration keeps the round trip working; the chat never sees the tool.
            Some((_, true)) => continue,
            None => return Err(invalid(format!("no handler for tool {name}"))),
        }
        let declared = ctx
            .permissions
            .ai_tool(name)
            .expect("the permission check proved the declaration");
        offered.push(ToolDef {
            name: name.clone(),
            description: declared.description.clone(),
            schema: declared.schema.clone(),
        });
    }
    Ok(offered)
}

/// What the producer does about the event it woke for: the stream broke, a deadline won, the
/// provider ended without completing, or there is an event to relay.
enum Step {
    Closed,
    TimedOut,
    Deadline,
    Ended,
    Event(ChatEvent),
}

/// Relays the events of the provider to the stream of the page until one of them ends the chat:
/// the page closes the iterator, a deadline of docs/stages/m9-ai.md §6 wins, or the provider
/// completes. Exactly one terminal outcome per chat, and nothing after it. A pending tool call
/// whose handler is too slow is left alone: the provider's own wait on the result fails the chat
/// with `Timeout`, and the deadlines above bound it all the same.
async fn produce(
    mut writer: StreamWriter,
    mut events: mpsc::Receiver<ChatEvent>,
    state: Arc<ChatState>,
    cancel: CancelSender,
    session: Arc<Session>,
    resource: ResourceId,
    host: Arc<dyn Host>,
    window: u64,
    chat: u64,
) {
    let overall = tokio::time::Instant::now() + limits::CHAT_TIME;
    let mut stage = tokio::time::Instant::now() + limits::FIRST_EVENT_TIME;
    loop {
        // The guard is dropped before the select; only the earliest deadline survives it.
        let next_deadline = {
            let pending = state.pending();
            pending.values().map(|call| call.deadline).min()
        };
        let step = {
            let closed = writer.closed();
            tokio::pin!(closed);
            tokio::select! {
                _ = &mut closed => Step::Closed,
                _ = async {
                    match next_deadline {
                        Some(at) => {
                            tokio::time::sleep_until(tokio::time::Instant::from_std(at)).await
                        }
                        None => std::future::pending::<()>().await,
                    }
                } => Step::Deadline,
                waited = tokio::time::timeout_at(stage.min(overall), events.recv()) => match waited {
                    Err(_) => Step::TimedOut,
                    Ok(None) => Step::Ended,
                    Ok(Some(event)) => Step::Event(event),
                },
            }
        };
        match step {
            // The page broke the iterator, or the session closed: nothing is written any more.
            Step::Closed => {
                abandon_chat(&state, &cancel, &session, resource, chat);
                return;
            }
            Step::TimedOut => {
                abandon_chat(&state, &cancel, &session, resource, chat);
                writer.error(timeout_of("the chat took too long"));
                return;
            }
            Step::Deadline => {
                // A result may have landed between the fire and here; only a real expiry fails.
                let expired = state
                    .pending()
                    .values()
                    .any(|call| call_expired(call.deadline, Instant::now()));
                if !expired {
                    continue;
                }
                abandon_chat(&state, &cancel, &session, resource, chat);
                writer.error(timeout_of("a tool handler took too long"));
                return;
            }
            Step::Ended => {
                abandon_chat(&state, &cancel, &session, resource, chat);
                writer.error(internal("the provider ended without completing"));
                return;
            }
            Step::Event(event) => {
                stage = tokio::time::Instant::now() + limits::STREAM_IDLE_TIME;
                match event {
                    ChatEvent::Text(text) => {
                        let frame = json!({"type": "text", "text": text});
                        if oversized(&frame) {
                            abandon_chat(&state, &cancel, &session, resource, chat);
                            writer.error(internal("the provider sent a part that is too large"));
                            return;
                        }
                        // A slow or gone page fails the whole chat.
                        if !relay(&writer, frame, limits::STREAM_IDLE_TIME).await {
                            abandon_chat(&state, &cancel, &session, resource, chat);
                            writer.error(timeout_of("the chat could not keep up with the page"));
                            return;
                        }
                    }
                    ChatEvent::ToolCall {
                        call_id,
                        name,
                        arguments,
                    } => {
                        let frame = json!({
                            "type": "tool_call",
                            "id": call_id,
                            "name": name,
                            "arguments": arguments,
                        });
                        if oversized(&frame) {
                            abandon_chat(&state, &cancel, &session, resource, chat);
                            writer.error(internal("the provider sent a part that is too large"));
                            return;
                        }
                        // The pending budget of the whole application (docs/stages/m9-ai.md §6).
                        let pending_calls = {
                            let all = chats();
                            all.get(&session.id().0)
                                .map(|own| {
                                    own.values()
                                        .map(|chat_state| chat_state.pending().len())
                                        .sum::<usize>()
                                })
                                .unwrap_or(0)
                        };
                        if pending_calls >= limits::MAX_PENDING_CALLS {
                            abandon_chat(&state, &cancel, &session, resource, chat);
                            writer.error(internal("too many pending tool calls"));
                            return;
                        }
                        state.pending().insert(
                            call_id.clone(),
                            PendingCall {
                                deadline: Instant::now() + limits::HANDLER_TIME,
                                name: name.clone(),
                            },
                        );
                        if !relay(&writer, frame, limits::STREAM_IDLE_TIME).await {
                            abandon_chat(&state, &cancel, &session, resource, chat);
                            writer.error(timeout_of("the chat could not keep up with the page"));
                            return;
                        }
                        // The event drives the page handler independently of the iterator.
                        host.emit(
                            Some(window),
                            "ai.tool_call",
                            json!({
                                "chat": chat,
                                "callId": call_id,
                                "name": name,
                                "arguments": arguments,
                            }),
                        );
                    }
                    ChatEvent::Usage(usage) => {
                        let frame = json!({
                            "type": "usage",
                            "inputTokens": usage.input_tokens,
                            "outputTokens": usage.output_tokens,
                            "cacheReadTokens": usage.cache_read_tokens,
                            "cacheCreationTokens": usage.cache_creation_tokens,
                            "estimated": usage.estimated,
                            "complete": usage.complete,
                        });
                        if !relay(&writer, frame, limits::STREAM_IDLE_TIME).await {
                            abandon_chat(&state, &cancel, &session, resource, chat);
                            writer.error(timeout_of("the chat could not keep up with the page"));
                            return;
                        }
                    }
                    ChatEvent::Completed => {
                        // Exactly one end (docs/stages/m9-ai.md §2): the frame, then the transport end.
                        if !relay(
                            &writer,
                            json!({"type": "end", "reason": "completed"}),
                            limits::STREAM_IDLE_TIME,
                        )
                        .await
                        {
                            abandon_chat(&state, &cancel, &session, resource, chat);
                            return;
                        }
                        state.fail();
                        let _ = session.resources().remove(resource);
                        forget_chat(session.id().0, chat);
                        writer.end();
                        return;
                    }
                }
            }
        }
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ChatArgs {
    role: String,
    messages: Vec<ChatMessage>,
    #[serde(default)]
    tools: Vec<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HandleArgs {
    name: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct UnhandleArgs {
    registration: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ResultArgs {
    chat: u64,
    call_id: String,
    /// The tool the caller claims the call was for; checked before the call is consumed.
    #[serde(default)]
    name: Option<String>,
    #[serde(default)]
    result: Option<Value>,
    #[serde(default)]
    error: Option<String>,
}

pub(crate) fn register(
    registry: &mut Registry,
    host: Arc<dyn Host>,
    context: &ModuleContext,
) -> Result<(), AlefError> {
    let provider = context.backends.ai.clone();

    // No permission on the wrapper: the handler publishes only the roles the user allowed; a
    // substituted role is not published (docs/stages/m9-ai.md §2).
    let roles_provider = provider.clone();
    registry
        .command::<()>("ai.roles")?
        .handler(move |ctx, _args| {
            let provider = roles_provider.clone();
            async move {
                let allowed: Vec<String> = ctx
                    .permissions
                    .ai_roles()
                    .iter()
                    .filter(|id| {
                        matches!(
                            ctx.permissions
                                .check(Permission::AiRole, Some(id), &ctx.grants()),
                            Ok(Decision::Allow)
                        )
                    })
                    .cloned()
                    .collect();
                let listed =
                    tokio::time::timeout(limits::ROLE_LIST_TIME, async move { provider.roles() })
                        .await
                        .map_err(|_| timeout_of("listing the roles took too long"))??;
                let roles: Vec<Role> = listed
                    .into_iter()
                    .filter(|role| allowed.contains(&role.id))
                    .collect();
                json(&json!({ "roles": roles }))
            }
        })?;

    let chat_provider = provider.clone();
    registry
        .command::<ChatArgs>("ai.chat")?
        .permission(Permission::AiRole, |args| Some(args.role.clone()))
        .substitutes()
        .waits()
        .handler(move |ctx, args| {
            let provider = chat_provider.clone();
            async move {
                // Nothing at all reaches the provider for a substituted role.
                if ctx.decision() == Decision::Substitute {
                    return Err(AlefError::new(
                        ErrorCode::NotAvailable,
                        "the role is not available",
                    ));
                }
                validate_chat(&args)?;
                let tools = allowed_tools(&ctx, &args)?;
                // The admission budgets of docs/stages/m9-ai.md §6, before anything opens.
                {
                    let mut all = chats();
                    if all.values().map(|own| own.len()).sum::<usize>() >= limits::MAX_CHATS {
                        return Err(busy("too many chats across the applications".to_owned()));
                    }
                    let own = all.entry(ctx.session.id().0).or_default();
                    if own.len() >= limits::MAX_CHATS_PER_APPLICATION {
                        return Err(busy("too many chats for this application".to_owned()));
                    }
                }
                let (cancel, cancelled) = CancelHandle::channel();
                let (results, result_receiver) = mpsc::channel(1);
                let events = provider.chat(
                    ChatRequest {
                        role: args.role.clone(),
                        messages: args.messages.clone(),
                        tools,
                    },
                    ChatControl {
                        cancel: cancelled,
                        results: result_receiver,
                    },
                )?;
                let (writer, stream_id) = ctx.streams().open_outgoing();
                let state = Arc::new(ChatState::new());
                let resource = ctx.resources().insert(Box::new(ChatHandle {
                    state: state.clone(),
                    cancel: cancel.clone(),
                    results,
                }))?;
                let chat = resource.0;
                // The chat is admitted; nothing after this can fail, so the entry is never stranded.
                chats()
                    .entry(ctx.session.id().0)
                    .or_default()
                    .insert(chat, state.clone());
                let window = ctx.session.window();
                let session = ctx.session.clone();
                tokio::spawn(produce(
                    writer,
                    events,
                    state,
                    cancel,
                    session,
                    resource,
                    host.clone(),
                    window,
                    chat,
                ));
                json(&json!({ "chat": chat, "stream": stream_id.0 }))
            }
        })?;

    registry
        .command::<HandleArgs>("ai.tools.handle")?
        .permission(Permission::AiTool, |args| Some(args.name.clone()))
        .substitutes()
        .handler(|ctx, args| async move {
            let inert = ctx.decision() == Decision::Substitute;
            let session = ctx.session.id().0;
            let mut registrations = tools();
            if registrations
                .values()
                .filter(|(who, _)| *who == session)
                .count()
                >= limits::MAX_TOOLS
            {
                return Err(busy("too many tool registrations".to_owned()));
            }
            if registrations.contains_key(&(session, args.name.clone())) {
                return Err(busy(format!(
                    "the tool {} is registered already",
                    args.name
                )));
            }
            let key = (session, args.name.clone());
            let id = ctx
                .resources()
                .insert(Box::new(Registration { key: key.clone() }))?;
            registrations.insert(key, (id.0, inert));
            drop(registrations);
            json(&json!({ "registration": id.0, "inert": inert }))
        })?;

    // Removing a registration is a plain handle: no right of its own, idempotent either way.
    registry
        .command::<UnhandleArgs>("ai.tools.unhandle")?
        .handler(|ctx, args| async move {
            let id = ResourceId(args.registration);
            let key = ctx
                .resources()
                .with_as::<Registration, _>(id, |registration| registration.key.clone());
            if let Ok((session, name)) = key {
                if session == ctx.session.id().0 {
                    if let Ok(resource) = ctx.resources().take(id) {
                        // The close of the resource removes it from the global map.
                        resource.close().await;
                    }
                    // The pending calls of the tool are invalidated: their later results answer CLOSED.
                    let mut all = chats();
                    if let Some(own) = all.get_mut(&session) {
                        for state in own.values() {
                            state.pending().retain(|_, call| call.name != name);
                        }
                    }
                }
            }
            Ok(Reply::Json(Value::Null))
        })?;

    registry
        .command::<ResultArgs>("ai.tools.result")?
        .handler(|ctx, args| async move {
            // Exactly one of the value and the failure, whatever the caller claims about the name.
            match (&args.result, &args.error) {
                (Some(_), None) | (None, Some(_)) => {}
                _ => {
                    return Err(invalid(
                        "a tool result carries its value or its failure, not both or neither"
                            .to_owned(),
                    ))
                }
            }
            if let Some(value) = &args.result {
                let bytes = serde_json::to_vec(value)
                    .map_err(|_| invalid("the result of the tool does not serialize".to_owned()))?;
                if bytes.len() > limits::MAX_TOOL_DATA_BYTES {
                    return Err(invalid(
                        "the result of the tool is longer than 64 KiB".to_owned(),
                    ));
                }
            }
            // A chat that is gone answers like a chat whose call is gone: never distinguish.
            let (state, results) = match ctx
                .resources()
                .with_as::<ChatHandle, _>(ResourceId(args.chat), |handle| {
                    (handle.state.clone(), handle.results.clone())
                }) {
                Ok(found) => found,
                Err(_) => return Err(closed_call()),
            };
            if state.cancelled() {
                return Err(closed_call());
            }
            // The stored binding is checked before the call is consumed: a mismatched name,
            // an expired deadline or an unknown call never burns the single-use claim.
            {
                let pending = state.pending();
                let Some(call) = pending.get(&args.call_id) else {
                    return Err(closed_call());
                };
                if let Some(provided) = &args.name {
                    if provided != &call.name {
                        return Err(closed_call());
                    }
                }
                if call_expired(call.deadline, Instant::now()) {
                    drop(pending);
                    state.pending().remove(&args.call_id);
                    return Err(closed_call());
                }
            }
            // Taking the pending call is the single-use claim: the same result never passes twice.
            if state.pending().remove(&args.call_id).is_none() {
                return Err(closed_call());
            }
            let payload = match (args.result, args.error) {
                (Some(value), _) => Ok(value),
                (None, Some(message)) => Err(AlefError::new(
                    ErrorCode::Internal,
                    format!("the tool handler failed: {}", sanitize_failure(&message)),
                )),
                _ => unreachable!("exactly one of result and error was checked above"),
            };
            // Forwarding to the provider; a full or dropped channel means the chat is over anyway.
            let _ = results.send(payload).await;
            Ok(Reply::Json(Value::Null))
        })?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_call_expires_strictly_after_its_deadline_not_at_it() {
        let now = Instant::now();
        assert!(!call_expired(now, now));
        assert!(call_expired(now, now + Duration::from_nanos(1)));
        assert!(!call_expired(now + Duration::from_secs(1), now));
    }

    #[test]
    fn a_failure_is_sized_and_printable() {
        assert_eq!(sanitize_failure("boom"), "boom");
        assert_eq!(sanitize_failure("a\nb\tc"), "abc");
        let long = "x".repeat(500);
        assert_eq!(sanitize_failure(&long).len(), 200);
    }
}
