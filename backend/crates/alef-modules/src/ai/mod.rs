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
    time::Instant,
};

use alef_core::{
    ids::ResourceId,
    registry::{
        command::Reply,
        context::{CallContext, CancelHandle, CancelSender},
        dispatch::Registry,
        host::Host,
    },
    security::{
        consent::Decision,
        permissions::{Grants, Permission, PermissionSet},
    },
    session::{resources::Resource, session::Session, streams::StreamWriter},
    AlefError, ErrorCode,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use tokio::sync::mpsc;

use crate::{json, ModuleContext};
use dto::{ChatMessage, Role};
use provider::{ChatControl, ChatEvent, ChatRequest, ToolDef};

pub mod dto;
pub mod limits;
pub mod provider;

/// A stable identity of one document session for the process-global maps: the address of its
/// state. Two sessions of one application never share it; a session that is gone leaves its
/// entries behind only until its resources are torn down.
fn session_key(session: &Arc<Session>) -> u64 {
    Arc::as_ptr(session) as usize as u64
}

/// One live tool registration: an application of one document session and the tool name, to the
/// registration resource id and whether it is inert (a substituted tool: registered, never
/// offered). A short lock, never held across an await. The key is the authority token of the
/// application when it has one, its manifest id otherwise: two applications never see each
/// other's tools, and two documents of one application never see each other's either.
type ToolKey = (String, u64, String);

static TOOLS: std::sync::LazyLock<Mutex<HashMap<ToolKey, (u64, bool)>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

fn tools() -> MutexGuard<'static, HashMap<ToolKey, (u64, bool)>> {
    TOOLS.lock().unwrap_or_else(|error| error.into_inner())
}

/// One live chat: an application of one document session and the chat resource id. A short lock,
/// never held across an await (docs/stages/m9-ai.md §6).
type ChatKey = (String, u64, u64);

static CHATS: std::sync::LazyLock<Mutex<HashMap<ChatKey, Arc<ChatState>>>> =
    std::sync::LazyLock::new(|| Mutex::new(HashMap::new()));

fn chats() -> MutexGuard<'static, HashMap<ChatKey, Arc<ChatState>>> {
    CHATS.lock().unwrap_or_else(|error| error.into_inner())
}

/// Forgets exactly one chat of one application of one document session.
fn forget_chat(app_key: &str, session: u64, chat: u64) {
    chats().remove(&(app_key.to_owned(), session, chat));
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
    /// The registration generation the call was dispatched against; a result after a
    /// re-registration of the same name is refused (a new id no longer satisfies it).
    registration: u64,
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
    key: ToolKey,
}

impl Resource for Registration {
    fn close(self: Box<Self>) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        tools().remove(&self.key);
        Box::pin(async {})
    }
}

/// Whether the chat still holds a resource in `session`.
pub fn chat_is_pending(session: &Session, chat: u64) -> bool {
    session
        .resources()
        .with_as::<ChatHandle, _>(ResourceId(chat), |_| ())
        .is_ok()
}

/// Whether the registration resource still exists in `session`.
pub fn tool_is_registered(session: &Session, registration: u64) -> bool {
    session
        .resources()
        .with_as::<Registration, _>(ResourceId(registration), |_| ())
        .is_ok()
}

/// What one relayed frame did: it went out, the stream or the page is gone, or the page was too
/// slow within the remaining budget.
enum Delivery {
    Sent,
    Closed,
    Slow,
}

async fn relay(writer: &StreamWriter, frame: Value, at: tokio::time::Instant) -> Delivery {
    match tokio::time::timeout_at(at, writer.send_json(frame)).await {
        Ok(Ok(())) => Delivery::Sent,
        Ok(Err(_)) => Delivery::Closed, // the stream or the page is gone; already terminal
        Err(_) => Delivery::Slow,
    }
}

/// The earliest deadline right now: the whole chat, the current stage and the earliest pending
/// handler.
fn earliest(
    overall: tokio::time::Instant,
    stage: tokio::time::Instant,
    state: &ChatState,
) -> tokio::time::Instant {
    let next = state.pending().values().map(|call| call.deadline).min();
    overall
        .min(stage)
        .min(next.map(tokio::time::Instant::from_std).unwrap_or(overall))
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

/// The tools of the chat that reach the provider: allowed, registered, and not inert, each with
/// the registration generation it was offered against. A substituted tool is left out, a denied
/// or undeclared one is refused, and a declared and allowed one without a live registration in
/// THIS document is an argument error before anything starts.
fn allowed_tools(
    ctx: &CallContext,
    args: &ChatArgs,
    app_key: &str,
    session: u64,
) -> Result<Vec<(ToolDef, u64)>, AlefError> {
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
        let registered = tools()
            .get(&(app_key.to_owned(), session, name.clone()))
            .copied();
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
        offered.push((
            ToolDef {
                name: name.clone(),
                description: declared.description.clone(),
                schema: declared.schema.clone(),
            },
            registered.expect("the match above proved it").0,
        ));
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
    writer: StreamWriter,
    mut events: mpsc::Receiver<ChatEvent>,
    state: Arc<ChatState>,
    cancel: CancelSender,
    session: Arc<Session>,
    resource: ResourceId,
    host: Arc<dyn Host>,
    window: u64,
    chat: u64,
    app_key: String,
    session_key: u64,
    offered: HashMap<String, u64>,
    permissions: Arc<PermissionSet>,
    grants: Arc<Grants>,
) {
    let overall = tokio::time::Instant::now() + limits::CHAT_TIME;
    let mut stage = tokio::time::Instant::now() + limits::FIRST_EVENT_TIME;
    // Notifies the page that the chat is over, then cancels the provider, deadens the pending
    // calls and forgets the resource. The event comes first: it has to reach the page even when
    // no iterator frame ever will.
    let abandon = |state: &ChatState| {
        host.emit(Some(window), "ai.chat_closed", json!({"chat": chat}));
        cancel.cancel();
        state.fail();
        let _ = session.resources().remove(resource);
        forget_chat(&app_key, session_key, chat);
    };
    loop {
        let step = {
            let closed = writer.closed();
            tokio::pin!(closed);
            tokio::select! {
                _ = &mut closed => Step::Closed,
                _ = async {
                    // The same earliest-deadline bound as every relay; without a pending call it
                    // waits forever (the stage and the overall deadlines bound the rest).
                    if state.pending().is_empty() {
                        std::future::pending::<()>().await;
                    } else {
                        tokio::time::sleep_until(earliest(overall, stage, &state)).await;
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
                abandon(&state);
                return;
            }
            Step::TimedOut => {
                abandon(&state);
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
                abandon(&state);
                writer.error(timeout_of("a tool handler took too long"));
                return;
            }
            Step::Ended => {
                abandon(&state);
                writer.error(internal("the provider ended without completing"));
                return;
            }
            Step::Event(event) => {
                stage = tokio::time::Instant::now() + limits::STREAM_IDLE_TIME;
                match event {
                    ChatEvent::Text(text) => {
                        let frame = json!({"type": "text", "text": text});
                        if oversized(&frame) {
                            abandon(&state);
                            writer.error(internal("the provider sent a part that is too large"));
                            return;
                        }
                        // A gone page is already terminal; a slow one fails the whole chat.
                        let at = earliest(overall, stage, &state);
                        match relay(&writer, frame, at).await {
                            Delivery::Sent => {}
                            Delivery::Closed => {
                                abandon(&state);
                                return;
                            }
                            Delivery::Slow => {
                                abandon(&state);
                                writer
                                    .error(timeout_of("the chat could not keep up with the page"));
                                return;
                            }
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
                            abandon(&state);
                            writer.error(internal("the provider sent a part that is too large"));
                            return;
                        }
                        // The binding of the chat start is re-checked at dispatch: a tool whose
                        // registration is gone or re-registered since the offer, or that no longer
                        // has the permission, is never admitted — a stale call of the provider
                        // cannot revive it.
                        let Some(&registration) = offered.get(&name) else {
                            abandon(&state);
                            writer.error(internal(
                                "a tool call arrived for a tool that is no longer offered",
                            ));
                            return;
                        };
                        let bound = tools().get(&(app_key.clone(), session_key, name.clone()))
                            == Some(&(registration, false))
                            && permissions.check(Permission::AiTool, Some(&name), &grants)
                                == Ok(Decision::Allow);
                        if !bound {
                            abandon(&state);
                            writer.error(internal(
                                "a tool call arrived for a tool that is no longer offered",
                            ));
                            return;
                        }
                        // The pending budget of the whole application across every document, and
                        // the admission of the call, happen in one critical section without an
                        // await (the lock order CHATS before `state.pending` is the one
                        // `ai.tools.unhandle` uses too).
                        let over = {
                            let mut all = chats();
                            let pending_calls = all
                                .keys()
                                .filter(|(who, _, _)| who == &app_key)
                                .map(|key| all[key].pending().len())
                                .sum::<usize>();
                            if pending_calls >= limits::MAX_PENDING_CALLS {
                                true
                            } else {
                                if let Some(chat_state) =
                                    all.get_mut(&(app_key.clone(), session_key, chat))
                                {
                                    chat_state.pending().insert(
                                        call_id.clone(),
                                        PendingCall {
                                            deadline: Instant::now() + limits::HANDLER_TIME,
                                            name: name.clone(),
                                            registration,
                                        },
                                    );
                                }
                                false
                            }
                        };
                        if over {
                            abandon(&state);
                            writer.error(internal("too many pending tool calls"));
                            return;
                        }
                        // A newly inserted handler deadline bounds its own dispatch relay too.
                        let at = earliest(overall, stage, &state);
                        match relay(&writer, frame, at).await {
                            Delivery::Sent => {}
                            Delivery::Closed => {
                                abandon(&state);
                                return;
                            }
                            Delivery::Slow => {
                                abandon(&state);
                                writer
                                    .error(timeout_of("the chat could not keep up with the page"));
                                return;
                            }
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
                        let at = earliest(overall, stage, &state);
                        match relay(&writer, frame, at).await {
                            Delivery::Sent => {}
                            Delivery::Closed => {
                                abandon(&state);
                                return;
                            }
                            Delivery::Slow => {
                                abandon(&state);
                                writer
                                    .error(timeout_of("the chat could not keep up with the page"));
                                return;
                            }
                        }
                    }
                    ChatEvent::Completed => {
                        // Exactly one end (docs/stages/m9-ai.md §2): the frame, then the transport end.
                        let at = earliest(overall, stage, &state);
                        match relay(&writer, json!({"type": "end", "reason": "completed"}), at)
                            .await
                        {
                            Delivery::Sent => {}
                            Delivery::Closed | Delivery::Slow => {
                                abandon(&state);
                                return;
                            }
                        }
                        // The chat is over: notify, cancel, deaden, forget.
                        abandon(&state);
                        writer.end();
                        return;
                    }
                }
            }
        }
    }
}

/// Whether a JSON field was present at all: absent is the default, an explicit `null` is
/// `Some(Value::Null)`.
fn deserialize_some<'de, T: Deserialize<'de>, D: serde::Deserializer<'de>>(
    value: D,
) -> Result<Option<T>, D::Error> {
    Ok(Some(T::deserialize(value)?))
}

#[derive(Debug, Serialize, Deserialize)]
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
    name: String,
    /// Absent is the default; an explicit `null` is a valid value.
    #[serde(default, deserialize_with = "deserialize_some")]
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
    // Every application is its own world: the authority token when the application is a package,
    // the manifest id otherwise. Tools and chats are keyed by it.
    let app_key: String = context
        .authority
        .package_token(&context.app.id)
        .unwrap_or_else(|| context.app.id.clone());

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

    let chat_app_key = app_key.clone();
    let chat_provider = provider.clone();
    registry
        .command::<ChatArgs>("ai.chat")?
        .permission(Permission::AiRole, |args| Some(args.role.clone()))
        .substitutes()
        .waits()
        .handler(move |ctx, args| {
            let provider = chat_provider.clone();
            let host = host.clone();
            let app_key = chat_app_key.clone();
            async move {
                // Nothing at all reaches the provider for a substituted role.
                if ctx.decision() == Decision::Substitute {
                    return Err(AlefError::new(
                        ErrorCode::NotAvailable,
                        "the role is not available",
                    ));
                }
                validate_chat(&args)?;
                let session_key = session_key(&ctx.session);
                let tools = allowed_tools(&ctx, &args, &app_key, session_key)?;
                let offered: HashMap<String, u64> = tools
                    .iter()
                    .map(|(tool, id)| (tool.name.clone(), *id))
                    .collect();
                let (cancel, cancelled) = CancelHandle::channel();
                let (results, result_receiver) = mpsc::channel(1);
                // The admission budgets of docs/stages/m9-ai.md §6, the start of the provider and
                // the recording of the chat happen in ONE critical section; it holds no `.await`,
                // and the producer started inside cannot reach the CHATS lock before it is
                // released (its first event is paced by the provider). A `?` drops the guard.
                let (writer, stream_id, events, resource, state) = {
                    let mut all = chats();
                    if all.len() >= limits::MAX_CHATS {
                        return Err(busy("too many chats across the applications".to_owned()));
                    }
                    if all.keys().filter(|(who, _, _)| who == &app_key).count()
                        >= limits::MAX_CHATS_PER_APPLICATION
                    {
                        return Err(busy("too many chats for this application".to_owned()));
                    }
                    let events = provider.chat(
                        ChatRequest {
                            role: args.role.clone(),
                            messages: args.messages.clone(),
                            tools: tools.into_iter().map(|(tool, _)| tool).collect(),
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
                    // The chat is admitted; the entry is never stranded.
                    all.insert((app_key.clone(), session_key, chat), state.clone());
                    (writer, stream_id, events, resource, state)
                };
                let chat = resource.0;
                let window = ctx.session.window();
                let session = ctx.session.clone();
                let permissions = ctx.permissions.clone();
                let grants = ctx.grants();
                tokio::spawn(produce(
                    writer,
                    events,
                    state,
                    cancel,
                    session,
                    resource,
                    host,
                    window,
                    chat,
                    app_key,
                    session_key,
                    offered,
                    permissions,
                    grants,
                ));
                json(&json!({ "chat": chat, "stream": stream_id.0 }))
            }
        })?;

    let handle_app_key = app_key.clone();
    registry
        .command::<HandleArgs>("ai.tools.handle")?
        .permission(Permission::AiTool, |args| Some(args.name.clone()))
        .substitutes()
        .handler(move |ctx, args| {
            let app_key = handle_app_key.clone();
            async move {
                let inert = ctx.decision() == Decision::Substitute;
                let session_key = session_key(&ctx.session);
                let mut registrations = tools();
                if registrations
                    .keys()
                    .filter(|(who, _, _)| who == &app_key)
                    .count()
                    >= limits::MAX_TOOLS
                {
                    return Err(busy("too many tool registrations".to_owned()));
                }
                if registrations.contains_key(&(app_key.clone(), session_key, args.name.clone())) {
                    return Err(busy(format!(
                        "the tool {} is registered already",
                        args.name
                    )));
                }
                let key = (app_key.clone(), session_key, args.name.clone());
                let id = ctx
                    .resources()
                    .insert(Box::new(Registration { key: key.clone() }))?;
                registrations.insert(key, (id.0, inert));
                drop(registrations);
                json(&json!({ "registration": id.0, "inert": inert }))
            }
        })?;

    // Removing a registration is a plain handle: no right of its own, idempotent either way.
    let unhandle_app_key = app_key.clone();
    registry
        .command::<UnhandleArgs>("ai.tools.unhandle")?
        .handler(move |ctx, args| {
            let app_key = unhandle_app_key.clone();
            async move {
                let session_key = session_key(&ctx.session);
                let id = ResourceId(args.registration);
                let key = ctx
                    .resources()
                    .with_as::<Registration, _>(id, |registration| registration.key.clone());
                if let Ok((who, of_session, name)) = key {
                    // Only the application that registered the tool, in this document, may remove
                    // it.
                    if who == app_key && of_session == session_key {
                        if let Ok(resource) = ctx.resources().take(id) {
                            // The close of the resource removes it from the global map.
                            resource.close().await;
                        }
                        // The pending calls of the tool are invalidated: their later results
                        // answer CLOSED.
                        let mut all = chats();
                        for ((who, of_session, _), state) in all.iter_mut() {
                            if who == &app_key && *of_session == session_key {
                                state.pending().retain(|_, call| call.name != name);
                            }
                        }
                    }
                }
                Ok(Reply::Json(Value::Null))
            }
        })?;

    let result_app_key = app_key.clone();
    registry
        .command::<ResultArgs>("ai.tools.result")?
        .handler(move |ctx, args| {
            let app_key = result_app_key.clone();
            async move {
                // Exactly one of the value and the failure.
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
                    let bytes = serde_json::to_vec(value).map_err(|_| {
                        invalid("the result of the tool does not serialize".to_owned())
                    })?;
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
                let session_key = session_key(&ctx.session);
                // The tool must still be registered and offered by this application of this
                // document; a result for a dead tool never burns the single-use claim.
                if !tools()
                    .get(&(app_key.clone(), session_key, args.name.clone()))
                    .is_some_and(|(_, inert)| !*inert)
                {
                    return Err(closed_call());
                }
                // Binding and consumption in one guard scope: a mismatched name, a stale
                // registration generation, an expired deadline or an unknown call never burns the
                // single-use claim; taking the entry is the claim.
                {
                    let mut pending = state.pending();
                    let Some(call) = pending.get(&args.call_id) else {
                        return Err(closed_call());
                    };
                    if args.name != call.name {
                        return Err(closed_call());
                    }
                    // A re-registered tool of the same name has a new generation; it no longer
                    // satisfies an old call.
                    if tools().get(&(app_key.clone(), session_key, args.name.clone()))
                        != Some(&(call.registration, false))
                    {
                        return Err(closed_call());
                    }
                    // The caller must still hold the right for the tool it answers for.
                    if ctx
                        .permissions
                        .check(Permission::AiTool, Some(&args.name), &ctx.grants())
                        != Ok(Decision::Allow)
                    {
                        return Err(closed_call());
                    }
                    if call_expired(call.deadline, Instant::now()) {
                        pending.remove(&args.call_id);
                        return Err(closed_call());
                    }
                    if pending.remove(&args.call_id).is_none() {
                        return Err(closed_call());
                    }
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
                match results.send(payload).await {
                    Ok(()) => Ok(Reply::Json(Value::Null)),
                    Err(_) => Err(closed_call()),
                }
            }
        })?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;

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
