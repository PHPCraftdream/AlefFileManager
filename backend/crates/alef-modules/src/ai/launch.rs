// SPDX-License-Identifier: MIT OR Apache-2.0
//! Admission and launch of an AI chat.
use super::*;

pub(super) async fn start_chat(
    ctx: CallContext,
    args: ChatArgs,
    provider: Arc<dyn provider::AiProvider>,
    host: Arc<dyn Host>,
    app_key: String,
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
        let events = provider.chat(
            ChatRequest {
                role: args.role.clone(),
                messages: args.messages.clone(),
                tools: tools.into_iter().map(|(tool, _)| tool).collect(),
                builtin: builtin_tools,
                history: turn.is_some(),
            },
            ChatControl {
                cancel: cancelled.clone(),
                results: result_receiver,
            },
        )?;
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
        offered,
        builtins,
        builtin_results,
        permissions,
        grants,
        turn,
    ));
    json(&json!({ "chat": chat, "stream": stream_id.0 }))
}
