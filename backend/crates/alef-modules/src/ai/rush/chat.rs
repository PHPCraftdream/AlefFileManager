// SPDX-License-Identifier: MIT OR Apache-2.0
use super::*;
use crate::ai::limits;
use crate::ai::provider::{
    fail_after, spawn_child, sync, ChatControl, ChatEvent, ChatMessage, ChatUsage, ProbeOutcome,
};
use crate::ai::rush::{
    DID_NOT_START, MAX_LINE, MAX_OUTPUT, MAX_TOKENS, SEND_GRACE, STDERR_CAP, TOO_LATE,
};
use crate::ai::{
    build_args, content_of, fail, oneshot_session, phase, probe_phase, remove_scratch,
    send_guarded, served_slots, session_id, ChatTask, Outcome, Phase,
};
use std::hash::{Hash, Hasher};
/// Chat producer.
pub(super) async fn run_chat(
    task: ChatTask,
    control: ChatControl,
    sender: mpsc::Sender<ChatEvent>,
) {
    let deadline = tokio::time::Instant::now() + task.chat_deadline;
    let send_deadline = || tokio::time::Instant::now() + SEND_GRACE;

    // Phase: the probe (inner-bounded by PROBE_TIME on the executor). Cancel ends silently —
    // no child exists yet; the deadline answers with exactly one Timeout event.
    let root = match tokio::select! {
        biased;
        _ = control.cancel.cancelled() => return,
        _ = tokio::time::sleep_until(deadline) => {
            fail_after(&control, &sender, ErrorCode::Timeout, TOO_LATE, tokio::time::Instant::now() + SEND_GRACE).await;
            return;
        }
        root = task.data_root.get() => root,
    } {
        Ok(root) => root,
        Err(error) => {
            fail(&control, &sender, deadline, error.code, DID_NOT_START).await;
            return;
        }
    };
    let probed = match probe_phase(
        &control,
        deadline,
        task.command.clone(),
        task.data_root.clone(),
    )
    .await
    {
        Phase::Done(Some(ProbeOutcome::Json(value))) => Ok(value),
        Phase::Done(Some(_)) | Phase::Done(None) => Err(()),
        Phase::Cancelled => return,
        Phase::Deadline => {
            fail_after(
                &control,
                &sender,
                ErrorCode::Timeout,
                TOO_LATE,
                send_deadline(),
            )
            .await;
            return;
        }
    };
    let served =
        matches!(&probed, Ok(value) if served_slots(value).contains(&task.request.role.as_str()));
    if !served {
        let (code, message) = match probed {
            Ok(_) => (
                ErrorCode::NotAvailable,
                "the role is not served by the rush provider",
            ),
            Err(()) => (ErrorCode::Internal, "the rush models probe failed"),
        };
        fail(&control, &sender, deadline, code, message).await;
        return;
    }
    if control.cancel.is_cancelled() {
        return;
    }
    let (id, evicted) = session_id(&task);
    if control.cancel.is_cancelled() {
        if !task.request.history {
            oneshot_session(&task, "delete", &id).await;
        }
        return;
    }
    if let Some(old) = evicted {
        delete_sessions_owned(task.data_root.clone(), task.command.clone(), vec![old]).await;
    }
    let scratch = root.join(format!("cwd-{id}"));

    // Phase: the scratch and data directories (blocking fs on the executor).
    let dirs = {
        let scratch = scratch.clone();
        let data_root = root.clone();
        phase(&control, deadline, move || {
            std::fs::create_dir(&scratch).is_ok() && data_root.is_dir()
        })
        .await
    };
    let dirs = match dirs {
        Phase::Cancelled => return,
        Phase::Deadline => {
            fail_after(
                &control,
                &sender,
                ErrorCode::Timeout,
                TOO_LATE,
                send_deadline(),
            )
            .await;
            return;
        }
        Phase::Done(ok) => ok.unwrap_or(false),
    };
    if !dirs {
        fail(
            &control,
            &sender,
            deadline,
            ErrorCode::Internal,
            DID_NOT_START,
        )
        .await;
        return;
    }
    let mut argv = task.command.prefix.clone();
    argv.extend(build_args(
        &task.request.role,
        &id,
        &scratch,
        &root,
        task.chat_deadline.as_secs().max(1),
        MAX_TOKENS,
        task.envelope,
    ));
    // The prompt is sent on stdin; history sends only its newest user turn.
    let prompt = if task.request.history {
        task.request
            .messages
            .iter()
            .rev()
            .find_map(|message| match message {
                ChatMessage::User { .. } => Some(content_of(message).to_owned()),
                _ => None,
            })
            .unwrap_or_default()
    } else {
        serde_json::to_string(&task.request.messages).unwrap_or_default()
    };

    // Phase: the program resolution (a stat per PATH candidate, on the executor).
    let program = {
        let command = task.command.clone();
        match phase(&control, deadline, move || sync::resolve(&command)).await {
            Phase::Done(program) => program.flatten(),
            Phase::Cancelled => return,
            Phase::Deadline => {
                fail_after(
                    &control,
                    &sender,
                    ErrorCode::Timeout,
                    TOO_LATE,
                    send_deadline(),
                )
                .await;
                return;
            }
        }
    };
    let resolved_program = match program {
        Some(program) => program,
        None => {
            fail(
                &control,
                &sender,
                deadline,
                ErrorCode::Internal,
                DID_NOT_START,
            )
            .await;
            return;
        }
    };

    let mut process = {
        let command = task.command.clone();
        let argv = argv.clone();
        let spawn_scratch = scratch.clone();
        match phase(&control, deadline, move || {
            spawn_child(&command, &resolved_program, &argv, &spawn_scratch)
        })
        .await
        {
            Phase::Done(process) => match process {
                Some(Ok(process)) => process,
                Some(Err(())) | None => {
                    remove_scratch(&scratch).await;
                    fail(
                        &control,
                        &sender,
                        deadline,
                        ErrorCode::Internal,
                        DID_NOT_START,
                    )
                    .await;
                    return;
                }
            },
            Phase::Cancelled => return,
            Phase::Deadline => {
                fail_after(
                    &control,
                    &sender,
                    ErrorCode::Timeout,
                    TOO_LATE,
                    send_deadline(),
                )
                .await;
                oneshot_session(&task, "cancel", &id).await;
                oneshot_session(&task, "delete", &id).await;
                return;
            }
        }
    };
    let killer = process.killer.clone();

    // Drain both output pipes while the prompt writer runs to avoid pipe deadlock.
    let mut stderr = process
        .stderr
        .take()
        .map(|stderr| tokio::spawn(async move { super::io::drain(stderr, STDERR_CAP).await }));
    let Some(stdout) = process.stdout.take() else {
        killer.kill();
        let _ = tokio::time::timeout(SEND_GRACE, process.child.wait()).await;
        return;
    };
    let stdout_pump = pump(&task, stdout, &control, &sender, deadline);
    tokio::pin!(stdout_pump);
    let mut write_task = process.stdin.take().map(|mut stdin| {
        let prompt = prompt.clone();
        tokio::spawn(async move {
            use tokio::io::AsyncWriteExt;
            let _ = stdin.write_all(prompt.as_bytes()).await;
            let _ = stdin.shutdown().await;
        })
    });
    let pump_and_write = async {
        let write_wait = async {
            if let Some(task) = write_task.as_mut() {
                let _ = task.await;
            }
        };
        let (pumped, _) = tokio::join!(&mut stdout_pump, write_wait);
        pumped
    };
    let phase_result = tokio::select! {
        biased;
        _ = control.cancel.cancelled() => Phase::Cancelled,
        _ = tokio::time::sleep_until(deadline) => Phase::Deadline,
        result = pump_and_write => Phase::Done(result),
    };
    let pumped = match phase_result {
        Phase::Done(result) => result,
        Phase::Cancelled => {
            killer.kill();
            let _ = tokio::time::timeout(SEND_GRACE, process.child.wait()).await;
            if let Some(task) = write_task.take() {
                let _ = tokio::time::timeout(SEND_GRACE, task).await;
            }
            drop(stderr);
            oneshot_session(&task, "cancel", &id).await;
            oneshot_session(&task, "delete", &id).await;
            remove_scratch(&scratch).await;
            return;
        }
        Phase::Deadline => {
            fail_after(
                &control,
                &sender,
                ErrorCode::Timeout,
                TOO_LATE,
                send_deadline(),
            )
            .await;
            killer.kill();
            let _ = tokio::time::timeout(SEND_GRACE, process.child.wait()).await;
            if let Some(task) = write_task.take() {
                let _ = tokio::time::timeout(SEND_GRACE, task).await;
            }
            drop(stderr);
            oneshot_session(&task, "cancel", &id).await;
            oneshot_session(&task, "delete", &id).await;
            remove_scratch(&scratch).await;
            return;
        }
    };
    let stderr_overflow = match stderr.as_mut() {
        Some(task) => match tokio::time::timeout(SEND_GRACE, task).await {
            Ok(Ok(overflow)) => overflow,
            _ => true,
        },
        None => false,
    };
    remove_scratch(&scratch).await;
    let pumped = if stderr_overflow {
        Pump::TooLarge
    } else {
        pumped
    };
    // Kill paths keep the tree armed; a natural exit disarms it (a recycled pid is never
    // signalled). Kill before disarm: disarm terminates what is still running.
    if let Pump::Ended(text_bytes, raw) = pumped {
        let status = tokio::select! {
            _ = control.cancel.cancelled() => {
                killer.kill();
                let _ = tokio::time::timeout(SEND_GRACE, process.child.wait()).await;
                                oneshot_session(&task, "cancel", &id).await;
                oneshot_session(&task, "delete", &id).await;
                remove_scratch(&scratch).await;
                return;
            }
            _ = tokio::time::sleep_until(deadline) => {
                fail_after(&control, &sender, ErrorCode::Timeout, TOO_LATE, send_deadline()).await;
                killer.kill();
                let _ = tokio::time::timeout(SEND_GRACE, process.child.wait()).await;
                                oneshot_session(&task, "cancel", &id).await;
                oneshot_session(&task, "delete", &id).await;
                remove_scratch(&scratch).await;
                return;
            }
            status = process.child.wait() => status,
        };
        killer.disarm();
        if !status.map(|status| status.success()).unwrap_or(false) {
            if !task.request.history {
                oneshot_session(&task, "delete", &id).await;
            }
            fail(
                &control,
                &sender,
                deadline,
                ErrorCode::Internal,
                "the rush run failed",
            )
            .await;
        } else if task.envelope {
            finish_envelope(&task, raw, &control, &sender, deadline, &id).await;
        } else {
            finish_stream(&task, text_bytes, &prompt, &control, &sender, deadline, &id).await;
        }
        return;
    }
    killer.kill();
    let _ = tokio::time::timeout(SEND_GRACE, process.child.wait()).await;
    match pumped {
        // The module already treats a cancelled chat as closed: end silently. Cancel first,
        // then the delete of a stateless chat; both best effort.
        Pump::Cancelled => {
            oneshot_session(&task, "cancel", &id).await;
            oneshot_session(&task, "delete", &id).await;
        }
        Pump::Deadline => {
            fail_after(
                &control,
                &sender,
                ErrorCode::Timeout,
                TOO_LATE,
                send_deadline(),
            )
            .await;
            oneshot_session(&task, "cancel", &id).await;
            oneshot_session(&task, "delete", &id).await;
        }
        Pump::TooLarge => {
            if !task.request.history {
                oneshot_session(&task, "delete", &id).await;
            }
            fail(
                &control,
                &sender,
                deadline,
                ErrorCode::Internal,
                "the rush answer is too large",
            )
            .await;
        }
        Pump::Ended(..) => {}
    }
}

async fn pump(
    task: &ChatTask,
    stdout: tokio::process::ChildStdout,
    control: &ChatControl,
    sender: &mpsc::Sender<ChatEvent>,
    deadline: tokio::time::Instant,
) -> Pump {
    use tokio::io::AsyncReadExt;
    let mut stdout = stdout;
    let mut chunk = vec![0_u8; 64 * 1024];
    let mut bounded = super::io::BoundedBytes::new(MAX_LINE, MAX_OUTPUT);
    let mut text_bytes = 0;
    let mut raw = Vec::new();
    loop {
        let read = tokio::select! { _ = control.cancel.cancelled() => return Pump::Cancelled, _ = tokio::time::sleep_until(deadline) => return Pump::Deadline, read = stdout.read(&mut chunk) => read };
        match read {
            Ok(0) | Err(_) => {
                if let Some(line) = bounded.finish() {
                    if task.envelope {
                        raw.extend_from_slice(&line);
                    } else {
                        let text = String::from_utf8_lossy(&line);
                        let text = text.trim_end_matches(['\n', '\r']);
                        if !text.is_empty() {
                            let text = format!("{text}\n");
                            text_bytes += text.len();
                            if send_guarded(sender, control, deadline, ChatEvent::Text(text)).await
                                != Outcome::Sent
                            {
                                return Pump::Cancelled;
                            }
                        }
                    }
                }
                return if bounded.overflowed() {
                    Pump::TooLarge
                } else {
                    Pump::Ended(text_bytes, raw)
                };
            }
            Ok(n) => {
                for line in bounded.push(&chunk[..n]) {
                    if task.envelope {
                        raw.extend_from_slice(&line);
                    } else {
                        let text = String::from_utf8_lossy(&line);
                        let text = text.trim_end_matches(['\n', '\r']);
                        if !text.is_empty() {
                            let text = format!("{text}\n");
                            text_bytes += text.len();
                            if send_guarded(sender, control, deadline, ChatEvent::Text(text)).await
                                != Outcome::Sent
                            {
                                return Pump::Cancelled;
                            }
                        }
                    }
                }
                if bounded.overflowed() {
                    return Pump::TooLarge;
                }
            }
        }
    }
}

/// How one stdout pump ends.
enum Pump {
    Cancelled,
    Deadline,
    TooLarge,
    Ended(usize, Vec<u8>),
}

/// Ends a chat that exited zero: final text (envelope), usage, completion, and the stateless
/// session delete once the outcome went out.
async fn finish_ok(
    task: &ChatTask,
    final_text: Option<String>,
    usage: ChatUsage,
    control: &ChatControl,
    sender: &mpsc::Sender<ChatEvent>,
    deadline: tokio::time::Instant,
    id: &str,
) {
    if let Some(text) = final_text {
        if send_guarded(sender, control, deadline, ChatEvent::Text(text)).await != Outcome::Sent {
            return;
        }
    }
    let completed = send_guarded(sender, control, deadline, ChatEvent::Usage(usage)).await
        == Outcome::Sent
        && send_guarded(sender, control, deadline, ChatEvent::Completed).await == Outcome::Sent;
    // A stateless chat that completed deletes its rush session; a history chat keeps the id.
    if completed && !task.request.history {
        oneshot_session(task, "delete", id).await;
    }
}

async fn finish_stream(
    task: &ChatTask,
    text_bytes: usize,
    prompt: &str,
    control: &ChatControl,
    sender: &mpsc::Sender<ChatEvent>,
    deadline: tokio::time::Instant,
    id: &str,
) {
    if text_bytes == 0 {
        if !task.request.history {
            oneshot_session(task, "delete", id).await;
        }
        fail(
            control,
            sender,
            deadline,
            ErrorCode::Internal,
            "the rush run wrote nothing",
        )
        .await;
        return;
    }
    finish_ok(
        task,
        None,
        ChatUsage {
            input_tokens: Some(prompt.len() as u64 / 4),
            output_tokens: Some(text_bytes as u64 / 4),
            cache_read_tokens: None,
            cache_creation_tokens: None,
            estimated: true,
            complete: true,
        },
        control,
        sender,
        deadline,
        id,
    )
    .await;
}

async fn finish_envelope(
    task: &ChatTask,
    raw: Vec<u8>,
    control: &ChatControl,
    sender: &mpsc::Sender<ChatEvent>,
    deadline: tokio::time::Instant,
    id: &str,
) {
    let parsed = serde_json::from_slice::<Value>(&raw)
        .ok()
        .and_then(|value| {
            let usage = value.get("usage");
            Some((
                value.get("final_text")?.as_str()?.to_owned(),
                usage
                    .and_then(|u| u.get("input_tokens"))
                    .and_then(Value::as_u64),
                usage
                    .and_then(|u| u.get("output_tokens"))
                    .and_then(Value::as_u64),
            ))
        });
    let Some((final_text, input, output)) = parsed else {
        if !task.request.history {
            oneshot_session(task, "delete", id).await;
        }
        fail(
            control,
            sender,
            deadline,
            ErrorCode::Internal,
            "the rush answer is unreadable",
        )
        .await;
        return;
    };
    if final_text.len() > limits::MAX_TEXT_PART_BYTES {
        if !task.request.history {
            oneshot_session(task, "delete", id).await;
        }
        fail(
            control,
            sender,
            deadline,
            ErrorCode::Internal,
            "the rush answer is too large",
        )
        .await;
        return;
    }
    finish_ok(
        task,
        Some(final_text),
        ChatUsage {
            input_tokens: input,
            output_tokens: output,
            cache_read_tokens: None,
            cache_creation_tokens: None,
            estimated: false,
            complete: true,
        },
        control,
        sender,
        deadline,
        id,
    )
    .await;
}

// ---------------------------------------------------------------------------
// The bounded pipe reader and the guarded failure send of the Rush bridge: module-shared
// plumbing used by the provider and its chat tasks.
// ---------------------------------------------------------------------------

/// A fresh session id: never reused within a run of the process.
pub(crate) fn fresh_session_id() -> String {
    let mut h = std::collections::hash_map::DefaultHasher::new();
    (
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_nanos())
            .unwrap_or_default(),
        super::SESSION_COUNTER.fetch_add(1, std::sync::atomic::Ordering::Relaxed),
    )
        .hash(&mut h);
    format!("alef-{:016x}", h.finish())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ai::provider::{effective_of, filter_env};

    fn argv(envelope: bool) -> Vec<String> {
        build_args(
            "smart",
            "alef-01",
            std::path::Path::new("/tmp/cwd"),
            std::path::Path::new("/tmp/data"),
            300,
            MAX_TOKENS,
            envelope,
        )
    }

    #[test]
    fn a_conversation_reuses_its_rush_session_and_a_different_one_never_shares() {
        // Mutation guard: content-keying merges these conversations.
        let sessions = Arc::new(Mutex::new(SessionMap::default()));
        let task_for = |conversation: Option<&str>| ChatTask {
            command: RushCommand {
                program: String::new(),
                prefix: Vec::new(),
            },
            data_root: Arc::new(ProviderRoot::new(PathBuf::new())),
            chat_deadline: Duration::from_secs(1),
            envelope: false,
            sessions: sessions.clone(),
            request: ChatRequest {
                role: "smart".to_owned(),
                messages: vec![ChatMessage::User {
                    content: "identical first message".to_owned(),
                }],
                tools: Vec::new(),
                builtin: Vec::new(),
                history: true,
                conversation: conversation.map(str::to_owned),
            },
        };
        let first = session_id(&task_for(Some("conv-1")));
        let second = session_id(&task_for(Some("conv-2")));
        assert_ne!(first, second);
        // Different ids for distinct conversations.
        let mut again = task_for(Some("conv-1"));
        again.request.messages.push(ChatMessage::User {
            content: "a different second message".to_owned(),
        });
        assert_eq!(session_id(&again), first);
        // Stateless calls always generate fresh ids.
        let mut stateless = task_for(None);
        stateless.request.history = false;
        assert_ne!(session_id(&stateless), first);
        assert_ne!(session_id(&stateless), session_id(&stateless));
    }

    #[test]
    fn the_environment_filter_drops_alef_variables_and_keeps_the_rest() {
        let filtered = filter_env(vec![
            ("PATH".to_owned(), "/bin".to_owned()),
            ("ALEF_TOKEN".to_owned(), "secret".to_owned()),
            ("alef_home".to_owned(), "secret".to_owned()),
            ("HOME".to_owned(), "/home".to_owned()),
        ]);
        assert_eq!(
            filtered,
            [
                ("PATH".to_owned(), "/bin".to_owned()),
                ("HOME".to_owned(), "/home".to_owned()),
            ]
        );
    }

    #[test]
    fn the_argv_carries_the_run_flags_and_never_the_prompt() {
        let args = argv(false);
        for flag in ["--restrict-run", "--max-tokens", "--timeout", "--stream"] {
            assert!(args.contains(&flag.to_owned()), "{flag}");
        }
        assert!(args.contains(&MAX_TOKENS.to_string()));
        assert!(args.contains(&"300".to_owned()));
        assert_eq!(args[args.len() - 1], "--stream");
        let envelope = argv(true);
        assert_eq!(envelope[envelope.len() - 1], "--json");
        // The prompt never rides in argv.
        assert!(args
            .iter()
            .chain(envelope.iter())
            .all(|arg| !arg.contains("secret prompt") && !arg.contains('\n')));
    }
    #[test]
    fn the_models_answer_is_reduced_to_its_effective_map() {
        // The real full shape: served_slots sees only the effective key's value.
        let full = serde_json::json!({
            "effective": {
                "smart": { "provider": "anthropic" },
                "fast": { "provider": "openai" },
            },
            "global": {},
            "local": {},
            "restore_commands": [],
        });
        let effective = effective_of(&full);
        assert_eq!(served_slots(&effective), ["smart", "fast"]);
        // An answer without the effective key serves nothing.
        assert_eq!(
            served_slots(&effective_of(&serde_json::json!({}))),
            Vec::<&str>::new()
        );
        // The reviewer rules hold on the reduced map: absent and explicit null serve smart,
        // an object reviewer refuses it.
        assert_eq!(served_slots(&effective), ["smart", "fast"]);
        let mut with_null = effective.clone();
        with_null["reviewer"] = Value::Null;
        assert_eq!(served_slots(&with_null), ["smart", "fast"]);
        let mut with_reviewer = effective;
        with_reviewer["reviewer"] = serde_json::json!({});
        assert_eq!(served_slots(&with_reviewer), ["fast"]);
    }
}
