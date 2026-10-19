// SPDX-License-Identifier: MIT OR Apache-2.0
//! Admission and launch of an AI chat.
use super::*;
use alef_core::registry::context::CancelHandle;
use provider::{ChatControl, ChatEvent, ChatRequest};

/// The user's quota and journal for this application, shared by every chat of the process.
pub(super) struct Usage {
    pub(super) store: alef_core::session::usage::Store,
    journal: alef_core::session::usage::Journal,
    /// The application id as the user sees it in the journal.
    app: String,
}

impl Usage {
    pub(super) fn new(folder: &std::path::Path, app: String) -> Self {
        Self {
            store: alef_core::session::usage::Store::new(folder),
            journal: alef_core::session::usage::Journal::new(folder),
            app,
        }
    }

    /// One journal line of this application, stamped now; failures are the caller's to gate on.
    /// The append is blocking file I/O: it runs on the bounded executor, never inline.
    pub(super) async fn record(
        &self,
        role: &str,
        kind: alef_core::session::usage::Kind,
        name: &str,
        event: alef_core::session::usage::Event,
        outcome: Option<&str>,
    ) -> Result<(), AlefError> {
        let entry = alef_core::session::usage::Entry {
            at: alef_core::session::usage::now_ms(),
            app: self.app.clone(),
            role: role.to_owned(),
            kind,
            name: name.to_owned(),
            event,
            outcome: outcome.map(str::to_owned),
        };
        let journal = self.journal.clone();
        let job = super::builtin::executor().submit(move || journal.append(&entry))?;
        // The executor is lost or the write failed: the journal text never leaks to the caller.
        match job.result().await {
            Ok(Ok(())) => Ok(()),
            Ok(Err(_)) | Err(_) => Err(AlefError::new(
                ErrorCode::Internal,
                "the journal is not available",
            )),
        }
    }
}

pub(super) async fn start_chat(
    ctx: CallContext,
    args: ChatArgs,
    provider: Arc<dyn provider::AiProvider>,
    host: Arc<dyn Host>,
    app_key: String,
    usage: std::sync::Arc<Usage>,
    turn: Option<sessions::Turn>,
) -> Result<Reply, AlefError> {
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
    let builtin_tools = builtin::offered(&ctx.permissions, &args.builtin)?;
    let builtins: HashSet<String> = builtin_tools.iter().map(|tool| tool.name.clone()).collect();
    let offered: HashMap<String, u64> = tools
        .iter()
        .map(|(tool, id)| (tool.name.clone(), *id))
        .collect();
    let request = ChatRequest {
        role: args.role.clone(),
        messages: args.messages.clone(),
        tools: tools.into_iter().map(|(tool, _)| tool).collect(),
        builtin: builtin_tools,
        history: turn.is_some(),
    };
    let bound = provider.token_bound(&request);
    // The counters write is disk work: it goes through the blocking executor.
    let store = usage.store.clone();
    let _reserved = super::builtin::executor()
        .submit(move || store.reserve(bound))?
        .result()
        .await?
        .map_err(|error| match error {
            alef_core::session::usage::Quota::Exceeded => AlefError::new(
                ErrorCode::QuotaExceeded,
                "the daily limit of this application is used up",
            ),
            alef_core::session::usage::Quota::Io(_) => {
                AlefError::new(ErrorCode::Internal, "the user quota is not available")
            }
        })?;
    let (cancel, cancelled) = CancelHandle::channel();
    if let Some(turn) = &turn {
        turn.attach_cancel(cancel.clone());
    }
    let (results, result_receiver) = mpsc::channel(limits::MAX_TOOLS);
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
        let events = provider
            .chat(
                request,
                ChatControl {
                    cancel: cancelled.clone(),
                    results: result_receiver,
                },
            )
            .map_err(cleanse)?;
        let (writer, stream_id) = ctx.streams().open_outgoing();
        let state = Arc::new(ChatState::new());
        let resource = ctx.resources().insert(Box::new(ChatHandle {
            state: state.clone(),
            cancel: cancel.clone(),
            results: results.clone(),
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
    let builtin_results = results.clone();
    tokio::spawn(produce(
        writer,
        events,
        state,
        cancelled,
        cancel,
        session,
        resource,
        host,
        window,
        chat,
        app_key,
        session_key,
        usage,
        args.role.clone(),
        offered,
        builtins,
        builtin_results,
        permissions,
        grants,
        turn,
    ));
    json(&json!({ "chat": chat, "stream": stream_id.0 }))
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

/// The tool calls that died with the chat get their journal outcome: they never answered.
async fn journal_cancelled(usage: &Usage, dropped: &[(String, String)]) {
    for (name, role) in dropped {
        let _ = usage
            .record(role, Kind::Tool, name, Event::End, Some("cancelled"))
            .await;
    }
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
    control: CancelHandle,
    cancel: CancelSender,
    session: Arc<Session>,
    resource: ResourceId,
    host: Arc<dyn Host>,
    window: u64,
    chat: u64,
    app_key: String,
    session_key: u64,
    usage: Arc<Usage>,
    role: String,
    offered: HashMap<String, u64>,
    builtins: HashSet<String>,
    builtin_results: mpsc::Sender<Result<Value, AlefError>>,
    permissions: Arc<PermissionSet>,
    grants: Arc<Grants>,
    mut turn: Option<sessions::Turn>,
) {
    let mut assistant = String::new();
    let overall = tokio::time::Instant::now() + limits::CHAT_TIME;
    let mut stage = tokio::time::Instant::now() + limits::FIRST_EVENT_TIME;
    // Notifies the page that the chat is over, then cancels the provider, deadens the pending
    // calls and forgets the resource. The event comes first: it has to reach the page even when
    // no iterator frame ever will. Returns the pending calls that died with the chat; the caller
    // journals their cancelled outcome (no lock held, no I/O inline).
    let abandon = |state: &ChatState| -> Vec<(String, String)> {
        host.emit(Some(window), "ai.chat_closed", json!({"chat": chat}));
        cancel.cancel();
        let dropped: Vec<(String, String)> = state
            .pending()
            .values()
            .map(|call| (call.name.clone(), call.role.clone()))
            .collect();
        state.fail();
        let _ = session.resources().remove(resource);
        forget_chat(&app_key, session_key, chat);
        dropped
    };
    loop {
        let step = {
            let closed = writer.closed();
            tokio::pin!(closed);
            tokio::select! {
                _ = &mut closed => Step::Closed,
                _ = control.cancelled() => Step::Closed,
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
                journal_cancelled(&usage, &abandon(&state)).await;
                writer.error(AlefError::new(ErrorCode::Closed, "the chat is closed"));
                return;
            }
            Step::TimedOut => {
                journal_cancelled(&usage, &abandon(&state)).await;
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
                journal_cancelled(&usage, &abandon(&state)).await;
                writer.error(timeout_of("a tool handler took too long"));
                return;
            }
            Step::Ended => {
                journal_cancelled(&usage, &abandon(&state)).await;
                writer.error(internal("the provider ended without completing"));
                return;
            }
            Step::Event(event) => {
                stage = tokio::time::Instant::now() + limits::STREAM_IDLE_TIME;
                match event {
                    ChatEvent::Text(text) => {
                        if let Some(turn) = &turn {
                            if turn.closed() {
                                journal_cancelled(&usage, &abandon(&state)).await;
                                return;
                            }
                        }
                        if turn.is_some()
                            && assistant.len().saturating_add(text.len())
                                > limits::MAX_MESSAGE_BYTES
                        {
                            journal_cancelled(&usage, &abandon(&state)).await;
                            writer.error(invalid("assistant response exceeds 256 KiB".to_owned()));
                            return;
                        }
                        if turn.is_some() {
                            assistant.push_str(&text);
                        }
                        let frame = json!({"type": "text", "text": text});
                        if oversized(&frame) {
                            journal_cancelled(&usage, &abandon(&state)).await;
                            writer.error(internal("the provider sent a part that is too large"));
                            return;
                        }
                        // A gone page is already terminal; a slow one fails the whole chat.
                        let at = earliest(overall, stage, &state);
                        match relay(&writer, frame, at).await {
                            Delivery::Sent => {}
                            Delivery::Closed => {
                                journal_cancelled(&usage, &abandon(&state)).await;
                                return;
                            }
                            Delivery::Slow => {
                                journal_cancelled(&usage, &abandon(&state)).await;
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
                        if serde_json::to_vec(&arguments)
                            .map_or(true, |bytes| bytes.len() > 64 * 1024)
                        {
                            journal_cancelled(&usage, &abandon(&state)).await;
                            writer.error(invalid("tool arguments exceed 64 KiB".to_owned()));
                            return;
                        }
                        if builtins.contains(&name) {
                            // The run is on the user's journal before anything happens.
                            if usage
                                .record(&role, Kind::Builtin, &name, Event::Start, None)
                                .await
                                .is_err()
                            {
                                journal_cancelled(&usage, &abandon(&state)).await;
                                writer.error(internal(
                                    "the journal refused the entry, the tool does not run",
                                ));
                                return;
                            }
                            let deadline = earliest(overall, stage, &state);
                            let result = {
                                let closed = writer.closed();
                                tokio::pin!(closed);
                                tokio::select! {
                                    biased;
                                    _ = control.cancelled() => Err(AlefError::new(ErrorCode::Closed, "the chat is closed")),
                                    _ = &mut closed => Err(AlefError::new(ErrorCode::Closed, "the chat is closed")),
                                    _ = tokio::time::sleep_until(deadline) => Err(timeout_of("a tool handler took too long")),
                                    result = builtin::execute(
                                        permissions.clone(),
                                        name.clone(),
                                        arguments,
                                        control.clone(),
                                    ) => result,
                                }
                            };
                            let value = match result {
                                Ok(value) => {
                                    let _ = usage
                                        .record(&role, Kind::Builtin, &name, Event::End, Some("ok"))
                                        .await;
                                    value
                                }
                                Err(error) => {
                                    let _ = usage
                                        .record(
                                            &role,
                                            Kind::Builtin,
                                            &name,
                                            Event::End,
                                            Some(
                                                if matches!(
                                                    error.code,
                                                    ErrorCode::Closed | ErrorCode::Timeout
                                                ) {
                                                    "cancelled"
                                                } else {
                                                    "error"
                                                },
                                            ),
                                        )
                                        .await;
                                    journal_cancelled(&usage, &abandon(&state)).await;
                                    writer.error(error);
                                    return;
                                }
                            };
                            match tokio::time::timeout_at(
                                earliest(overall, stage, &state),
                                builtin_results.send(Ok(value)),
                            )
                            .await
                            {
                                Ok(Ok(())) => {}
                                _ => {
                                    journal_cancelled(&usage, &abandon(&state)).await;
                                    writer.error(timeout_of("a tool handler took too long"));
                                    return;
                                }
                            }
                            continue;
                        }
                        let frame = json!({
                            "type": "tool_call",
                            "id": call_id,
                            "name": name,
                            "arguments": arguments,
                        });
                        if oversized(&frame) {
                            journal_cancelled(&usage, &abandon(&state)).await;
                            writer.error(internal("the provider sent a part that is too large"));
                            return;
                        }
                        // The binding of the chat start is re-checked at dispatch: a tool whose
                        // registration is gone or re-registered since the offer, or that no longer
                        // has the permission, is never admitted — a stale call of the provider
                        // cannot revive it.
                        let Some(&registration) = offered.get(&name) else {
                            journal_cancelled(&usage, &abandon(&state)).await;
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
                            journal_cancelled(&usage, &abandon(&state)).await;
                            writer.error(internal(
                                "a tool call arrived for a tool that is no longer offered",
                            ));
                            return;
                        }
                        // The journal carries the run before anything of the call reaches the
                        // page or the provider wait.
                        if usage
                            .record(&role, Kind::Tool, &name, Event::Start, None)
                            .await
                            .is_err()
                        {
                            journal_cancelled(&usage, &abandon(&state)).await;
                            writer.error(internal(
                                "the journal refused the entry, the tool does not run",
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
                                            role: role.clone(),
                                        },
                                    );
                                }
                                false
                            }
                        };
                        if over {
                            journal_cancelled(&usage, &abandon(&state)).await;
                            writer.error(internal("too many pending tool calls"));
                            return;
                        }
                        // A newly inserted handler deadline bounds its own dispatch relay too.
                        let at = earliest(overall, stage, &state);
                        match relay(&writer, frame, at).await {
                            Delivery::Sent => {}
                            Delivery::Closed => {
                                journal_cancelled(&usage, &abandon(&state)).await;
                                return;
                            }
                            Delivery::Slow => {
                                journal_cancelled(&usage, &abandon(&state)).await;
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
                    ChatEvent::Usage(chat_usage) => {
                        let frame = json!({
                            "type": "usage",
                            "inputTokens": chat_usage.input_tokens,
                            "outputTokens": chat_usage.output_tokens,
                            "cacheReadTokens": chat_usage.cache_read_tokens,
                            "cacheCreationTokens": chat_usage.cache_creation_tokens,
                            "estimated": chat_usage.estimated,
                            "complete": chat_usage.complete,
                        });
                        let at = earliest(overall, stage, &state);
                        match relay(&writer, frame, at).await {
                            Delivery::Sent => {}
                            Delivery::Closed => {
                                journal_cancelled(&usage, &abandon(&state)).await;
                                return;
                            }
                            Delivery::Slow => {
                                journal_cancelled(&usage, &abandon(&state)).await;
                                writer
                                    .error(timeout_of("the chat could not keep up with the page"));
                                return;
                            }
                        }
                    }
                    ChatEvent::Completed => {
                        // Exactly one end (docs/stages/m9-ai.md §2): the frame, then the transport end.
                        let at = earliest(overall, stage, &state);
                        if let Err(error) =
                            sessions::finish_turn(&writer, turn.take(), assistant, at).await
                        {
                            journal_cancelled(&usage, &abandon(&state)).await;
                            writer.error(error);
                            return;
                        }
                        // The chat is over: notify, cancel, deaden, forget.
                        journal_cancelled(&usage, &abandon(&state)).await;
                        writer.end();
                        return;
                    }
                }
            }
        }
    }
}
