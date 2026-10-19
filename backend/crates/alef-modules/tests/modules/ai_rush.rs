// SPDX-License-Identifier: MIT OR Apache-2.0
//! The `RushCliProvider` bridge driven directly, without the registry: a Node emulator stands in
//! for the user's `rush` CLI (tests/modules/rush_emulator.mjs) and every child it starts is
//! recorded under the `--data-dir` the provider passes. This is an integration test of the
//! bridge itself: allowlist, stdin prompt transport, tree kill, cleaned errors, sessions.
use std::{path::Path, time::Duration};

use alef_core::{
    registry::context::{CancelHandle, CancelSender},
    ErrorCode,
};
use alef_modules::{
    AiProvider, ChatControl, ChatEvent, ChatMessage, ChatRequest, ChatUsage, RushCliProvider,
    RushCommand, ToolDef,
};
use tokio::sync::mpsc;

/// The emulator script the provider runs through `node`.
fn script() -> String {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/modules/rush_emulator.mjs")
        .to_string_lossy()
        .into_owned()
}

/// A provider for one emulator mode, with its recording directory.
fn provider(mode: &str, deadline_ms: u64, envelope: bool) -> (RushCliProvider, tempfile::TempDir) {
    let dir = tempfile::tempdir().expect("tempdir");
    let command = RushCommand {
        program: "node".to_owned(),
        prefix: vec![script(), mode.to_owned()],
    };
    let inner = RushCliProvider::new(command, dir.path().to_path_buf())
        .with_chat_deadline(Duration::from_millis(deadline_ms))
        .with_envelope(envelope);
    (inner, dir)
}

fn request(role: &str, content: &str, history: bool, conversation: Option<&str>) -> ChatRequest {
    ChatRequest {
        role: role.to_owned(),
        messages: vec![ChatMessage::User {
            content: content.to_owned(),
        }],
        tools: Vec::new(),
        builtin: Vec::new(),
        history,
        conversation: conversation.map(str::to_owned),
    }
}

/// A chat control and its cancel handle.
fn control() -> (ChatControl, CancelSender) {
    let (cancel, cancelled) = CancelHandle::channel();
    let (_results, result_receiver) = mpsc::channel(4);
    (
        ChatControl {
            cancel: cancelled,
            results: result_receiver,
        },
        cancel,
    )
}

/// Receives events through a terminal event and requires the provider to close its channel.
async fn collect(
    mut receiver: mpsc::Receiver<ChatEvent>,
    max_events: usize,
    per_event: Duration,
) -> Vec<ChatEvent> {
    let mut events = Vec::new();
    for _ in 0..max_events {
        let event = tokio::time::timeout(per_event, receiver.recv())
            .await
            .expect("provider event arrived before timeout");
        match event {
            Some(event) => {
                let terminal = matches!(event, ChatEvent::Completed | ChatEvent::Failed { .. });
                events.push(event);
                if terminal {
                    assert!(
                        tokio::time::timeout(per_event, receiver.recv())
                            .await
                            .expect("provider closed after terminal event")
                            .is_none(),
                        "provider emitted after terminal event"
                    );
                    return events;
                }
            }
            None => return events,
        }
    }
    panic!("provider emitted more than {max_events} nonterminal events");
}

/// Whether the process is gone: the pid is polled until it answers no longer.
#[cfg(windows)]
async fn pid_gone(pid: u32, timeout: Duration) -> bool {
    use windows_sys::Win32::Foundation::{CloseHandle, WAIT_OBJECT_0};
    use windows_sys::Win32::System::Threading::{
        OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE,
    };
    let end = tokio::time::Instant::now() + timeout;
    loop {
        // SAFETY: the pid was recorded by the child itself; only synchronize access is asked.
        let handle = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
        // SAFETY: the wait probes with a zero timeout and the handle is live or null.
        let signaled =
            !handle.is_null() && unsafe { WaitForSingleObject(handle, 0) } == WAIT_OBJECT_0;
        if !handle.is_null() {
            // SAFETY: the handle came from a successful OpenProcess and is closed exactly once.
            unsafe { CloseHandle(handle) };
        }
        if handle.is_null() || signaled || tokio::time::Instant::now() >= end {
            return handle.is_null() || signaled;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

/// Whether the process is gone: the pid is polled until it answers no longer.
#[cfg(unix)]
async fn pid_gone(pid: u32, timeout: Duration) -> bool {
    let end = tokio::time::Instant::now() + timeout;
    loop {
        // SAFETY: signal zero only probes for the existence of the recorded child.
        if unsafe { libc::kill(pid as i32, 0) } == -1
            && std::io::Error::last_os_error().kind() == std::io::ErrorKind::NotFound
        {
            return true;
        }
        if tokio::time::Instant::now() >= end {
            return false;
        }
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

fn read_data(dir: &Path, name: &str) -> Vec<u8> {
    std::fs::read(dir.join(name)).expect("the recording file exists")
}

/// The recorded text, empty while the file does not exist yet (the polling loops wait for it).
fn read_text(dir: &Path, name: &str) -> String {
    String::from_utf8_lossy(&std::fs::read(dir.join(name)).unwrap_or_default()).into_owned()
}

/// The recorded argv arrays; each element is one invocation of the emulator.
fn argv_list(dir: &Path) -> Vec<Vec<String>> {
    serde_json::from_slice(&read_data(dir, "argv.json")).expect("argv.json parses")
}

/// The single `run` invocation's argv (argv[2] is the mode word).
fn run_argv(dir: &Path) -> Vec<String> {
    argv_list(dir)
        .into_iter()
        .find(|argv| argv.get(3).map(String::as_str) == Some("run"))
        .expect("a run invocation was recorded")
}

/// The value the provider passed as `--session` in the run invocation at `index`.
fn session_of_at(dir: &Path, index: usize) -> String {
    let argv = argv_list(dir)
        .into_iter()
        .filter(|argv| argv.get(3).map(String::as_str) == Some("run"))
        .nth(index)
        .expect("that many run invocations");
    let at = argv
        .iter()
        .position(|arg| arg == "--session")
        .expect("--session");
    argv[at + 1].clone()
}

/// The value the provider passed as `--session` in the first run invocation.
fn session_of(dir: &Path) -> String {
    session_of_at(dir, 0)
}

fn failed(code: ErrorCode, message: &str) -> ChatEvent {
    ChatEvent::Failed {
        code,
        message: message.to_owned(),
    }
}

// Mutation guard: removing the `served_slots` allowlist check (the `ALLOWED_PROVIDERS` match in
// src/ai/provider.rs) makes this test fail because the wrapper mode would stream like normal.
#[tokio::test]
async fn roles_follow_the_allowlist_and_refuse_a_wrapper_provider() {
    let (bridge, dir) = provider("normal", 8_000, false);
    let root = bridge.data_dir().await.expect("private data dir");
    assert!(root.starts_with(dir.path()));
    assert!(root.ends_with(".rush"));
    let roles = bridge.roles_fut().await.expect("roles");
    assert_eq!(
        roles
            .iter()
            .map(|role| role.id.as_str())
            .collect::<Vec<_>>(),
        ["smart", "fast"]
    );
    drop(bridge);

    let (bridge, dir) = provider("wrapper", 8_000, false);
    assert_eq!(bridge.roles_fut().await.expect("roles"), Vec::new());
    let (chat, _cancel) = control();
    let receiver = bridge
        .chat(request("smart", "probe", false, None), chat)
        .expect("chat starts");
    let events = collect(receiver, 4, Duration::from_secs(5)).await;
    assert_eq!(
        events,
        vec![failed(
            ErrorCode::NotAvailable,
            "the role is not served by the rush provider"
        )]
    );
    // Only the container of the provider itself may be left in the parent.
    assert!(std::fs::read_dir(dir.path())
        .expect("parent")
        .filter_map(Result::ok)
        .all(|entry| entry
            .file_name()
            .to_string_lossy()
            .starts_with("alef-provider-")));
}

// Mutation guard: dropping the `reviewer` rule in `served_slots` (src/ai/provider.rs) makes this
// test fail because smart would be served and stream while a reviewer pass watches the chat.
#[tokio::test]
async fn a_reviewer_slot_leaves_only_fast_served() {
    let (bridge, _dir) = provider("reviewer", 8_000, false);
    let roles = bridge.roles_fut().await.expect("roles");
    assert_eq!(
        roles
            .iter()
            .map(|role| role.id.as_str())
            .collect::<Vec<_>>(),
        ["fast"]
    );
    let (chat, _cancel) = control();
    let receiver = bridge
        .chat(request("smart", "reviewed", false, None), chat)
        .expect("chat starts");
    assert_eq!(
        collect(receiver, 4, Duration::from_secs(5)).await,
        vec![failed(
            ErrorCode::NotAvailable,
            "the role is not served by the rush provider"
        )]
    );
    let (chat, _cancel) = control();
    let receiver = bridge
        .chat(request("fast", "quick", false, None), chat)
        .expect("chat starts");
    let events = collect(receiver, 8, Duration::from_secs(5)).await;
    assert!(events.contains(&ChatEvent::Completed));
    assert!(events.iter().any(|event| matches!(
        event,
        ChatEvent::Usage(usage) if usage.estimated
    )));
}

// Mutation guard: removing the stream-mode Text relaying in `pump` (src/ai/rush.rs) makes this
// test fail because the three lines would never arrive as text parts.
#[tokio::test]
async fn a_chat_streams_text_parts_and_ends_exactly_once() {
    let (bridge, _dir) = provider("normal", 8_000, false);
    let (chat, _cancel) = control();
    let receiver = bridge
        .chat(request("smart", "stream me", false, None), chat)
        .expect("chat starts");
    let mut events = collect(receiver, 8, Duration::from_secs(5)).await;
    let completed = events.pop().expect("a terminal event");
    assert_eq!(completed, ChatEvent::Completed);
    let usage = events.pop().expect("a usage event");
    assert!(matches!(
        usage,
        ChatEvent::Usage(ChatUsage {
            estimated: true,
            complete: true,
            output_tokens: Some(4),
            ..
        })
    ));
    assert_eq!(
        events,
        vec![
            ChatEvent::Text("hello\n".to_owned()),
            ChatEvent::Text("world\n".to_owned()),
            ChatEvent::Text("done\n".to_owned()),
        ]
    );
}

// Mutation guard: taking the envelope usage fields instead of the envelope's own numbers (in
// `finish_envelope`, src/ai/rush.rs) makes this test fail because 11/7 would become estimates.
#[tokio::test]
async fn an_envelope_chat_reports_the_exact_usage_of_the_envelope() {
    let (bridge, _dir) = provider("normal", 8_000, true);
    let (chat, _cancel) = control();
    let receiver = bridge
        .chat(request("fast", "RUSH-ENVELOPE-USAGE", false, None), chat)
        .expect("chat starts");
    let serialized = serde_json::to_string(&vec![ChatMessage::User {
        content: "RUSH-ENVELOPE-USAGE".into(),
    }])
    .unwrap();
    let events = collect(receiver, 8, Duration::from_secs(5)).await;
    assert_eq!(events[0], ChatEvent::Text(serialized));
    assert_eq!(
        &events[1..],
        &[
            ChatEvent::Usage(ChatUsage {
                input_tokens: Some(11),
                output_tokens: Some(7),
                cache_read_tokens: None,
                cache_creation_tokens: None,
                estimated: false,
                complete: true,
            }),
            ChatEvent::Completed,
        ]
    );
}

#[tokio::test]
async fn a_envelope_prompt_is_the_serialized_message_array() {
    let (bridge, _dir) = provider("normal", 8_000, true);
    let root = bridge.data_dir().await.expect("provider root");
    let messages = vec![ChatMessage::User {
        content: "RUSH-ENVELOPE-USAGE".into(),
    }];
    let expected = serde_json::to_string(&messages).unwrap();
    let (chat, _cancel) = control();
    let receiver = bridge
        .chat(request("fast", "RUSH-ENVELOPE-USAGE", false, None), chat)
        .expect("chat starts");
    let events = collect(receiver, 8, Duration::from_secs(5)).await;
    assert_eq!(events[0], ChatEvent::Text(expected));
    let run = run_argv(&root);
    assert!(run.iter().any(|arg| arg == "--json"));
}

// test fail because a child would start and the recordings would appear under the data dir.
#[tokio::test]
async fn tools_and_builtins_answer_not_available_without_running_a_child() {
    for tools in [
        vec![ToolDef {
            name: "read".to_owned(),
            description: "reads".to_owned(),
            schema: serde_json::json!({ "type": "object" }),
        }],
        Vec::new(),
    ] {
        let builtin = if tools.is_empty() {
            vec![ToolDef {
                name: "fs".to_owned(),
                description: "builtin".to_owned(),
                schema: serde_json::json!({ "type": "object" }),
            }]
        } else {
            Vec::new()
        };
        let (bridge, dir) = provider("normal", 8_000, false);
        let (chat, _cancel) = control();
        let error = bridge
            .chat(
                ChatRequest {
                    role: "smart".to_owned(),
                    messages: vec![ChatMessage::User {
                        content: "with tools".to_owned(),
                    }],
                    tools: tools.clone(),
                    builtin,
                    history: false,
                    conversation: None,
                },
                chat,
            )
            .expect_err("tools are refused");
        assert_eq!(error.code, ErrorCode::NotAvailable);
        assert_eq!(error.message, "rush cannot take application tools yet");
        assert!(!std::fs::read_dir(dir.path())
            .expect("parent remains empty")
            .any(|entry| entry.is_ok()));
    }
}

// Mutation guard: removing the `killer.kill()` on the cancel path of `run_chat` (src/ai/rush.rs)
// makes this test fail because the hung child would outlive the cancel past the pid poll.
#[tokio::test]
async fn a_cancel_kills_the_child_tree_and_cancels_the_rush_session() {
    let (bridge, _dir) = provider("hang", 30_000, false);
    let root = bridge.data_dir().await.expect("provider root");
    let (chat, cancel) = control();
    let receiver = bridge
        .chat(request("smart", "hung", false, None), chat)
        .expect("chat starts");
    let end = tokio::time::Instant::now() + Duration::from_secs(5);
    let (pid, descendant_pid) = loop {
        if let (Ok(pid), Ok(descendant)) = (
            std::fs::read_to_string(root.join("run.pid")),
            std::fs::read_to_string(root.join("descendant.pid")),
        ) {
            break (
                pid.trim().parse::<u32>().expect("a pid"),
                descendant.trim().parse::<u32>().expect("a descendant pid"),
            );
        }
        assert!(tokio::time::Instant::now() < end, "no child tree started");
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    cancel.cancel();
    assert!(pid_gone(pid, Duration::from_secs(5)).await);
    assert!(
        pid_gone(descendant_pid, Duration::from_secs(5)).await,
        "the descendant survived tree cancellation"
    );
    let end = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        if read_text(&root, "sessions.log").contains("cancel alef-") {
            break;
        }
        assert!(
            tokio::time::Instant::now() < end,
            "the rush session was not cancelled: {}",
            read_text(&root, "sessions.log")
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    // After the cancel nothing new may arrive, and the channel closes without a Failed event.
    let text = collect(receiver, 4, Duration::from_millis(500)).await;
    assert!(
        text.is_empty(),
        "no failure expected after cancel: {text:?}"
    );
    // collect owns the receiver, so a closed receiver returns immediately on the next recv.
}

// Mutation guard: removing the `Pump::Deadline` kill-and-fail path in `run_chat` (src/ai/rush.rs)
// makes this test fail because the hung child would survive and no Timeout event would arrive.
#[tokio::test]
async fn a_hang_past_the_deadline_is_one_timeout_error_and_a_dead_child() {
    let (bridge, _dir) = provider("hang", 600, false);
    let root = bridge.data_dir().await.expect("provider root");
    let (chat, _cancel) = control();
    let receiver = bridge
        .chat(request("smart", "deadline", false, None), chat)
        .expect("chat starts");
    let events = collect(receiver, 4, Duration::from_secs(5)).await;
    assert_eq!(
        events,
        vec![failed(ErrorCode::Timeout, "the rush run took too long")]
    );
    let pid: u32 = read_text(&root, "run.pid").trim().parse().expect("a pid");
    assert!(pid_gone(pid, Duration::from_secs(5)).await);
}

// Mutation guard: replacing any fixed failure message with the child's own stderr text (the
// fail paths in src/ai/rush.rs) makes this test fail because the sentinel would surface.
#[tokio::test]
async fn garbage_nonzero_exit_and_oversize_are_cleaned_errors_without_child_text() {
    let cases = [
        (
            "garbage",
            true,
            failed(ErrorCode::Internal, "the rush answer is unreadable"),
        ),
        (
            "fail",
            false,
            failed(ErrorCode::Internal, "the rush run failed"),
        ),
        (
            "oversize",
            false,
            failed(ErrorCode::Internal, "the rush answer is too large"),
        ),
    ];
    for (mode, envelope, expected) in cases {
        let (bridge, _dir) = provider(mode, 8_000, envelope);
        let root = bridge.data_dir().await.expect("provider root");
        let (chat, _cancel) = control();
        let receiver = bridge
            .chat(request("smart", "dirty", false, None), chat)
            .expect("chat starts");
        let events = collect(receiver, 4, Duration::from_secs(5)).await;
        assert_eq!(events, vec![expected]);
        let dumped = format!("{events:?}");
        for secret in ["SECRET", ".rush", "sentinel"] {
            assert!(!dumped.contains(secret), "{secret} surfaced: {dumped}");
        }
        // The stateless delete also runs on failure paths: bounded poll for the recording.
        let end = tokio::time::Instant::now() + Duration::from_secs(5);
        loop {
            if read_text(&root, "sessions.log").contains("delete alef-") {
                break;
            }
            assert!(
                tokio::time::Instant::now() < end,
                "no stateless delete for {mode}: {}",
                read_text(&root, "sessions.log")
            );
            tokio::time::sleep(Duration::from_millis(50)).await;
        }
    }
}

// Mutation guard: putting the prompt into the argv instead of stdin (in `run_chat`,
// src/ai/rush.rs) makes this test fail because the sentinel would appear in argv.json.
#[tokio::test]
async fn the_prompt_travels_on_stdin_and_never_in_argv() {
    let (bridge, _dir) = provider("normal", 8_000, true);
    let root = bridge.data_dir().await.expect("provider root");
    let (chat, _cancel) = control();
    let receiver = bridge
        .chat(
            request("fast", "RUSH-PROMPT-SENTINEL-7f3a", false, None),
            chat,
        )
        .expect("chat starts");
    let events = collect(receiver, 8, Duration::from_secs(5)).await;
    assert!(
        matches!(&events[0], ChatEvent::Text(text) if text.contains("RUSH-PROMPT-SENTINEL-7f3a"))
    );
    let argv = serde_json::to_string(&argv_list(&root)).expect("argv.json");
    assert!(!argv.contains("RUSH-PROMPT-SENTINEL-7f3a"));
    let run = run_argv(&root);
    for flag in [
        "--restrict-run",
        "--max-tokens",
        "8192",
        "--json",
        "--session",
    ] {
        assert!(run.contains(&flag.to_owned()), "{flag} missing");
    }
    let session = session_of(&root);
    assert!(session.starts_with("alef-"));
}

// Mutation guard: keying the session map on anything but the conversation id (in `session_id`,
// src/ai/rush.rs) makes this test fail: identical first messages would share one rush session,
// or different conversations would not be deleted separately.
#[tokio::test]
async fn two_conversations_with_identical_first_messages_get_different_sessions() {
    let (bridge, _dir) = provider("normal", 8_000, false);
    let root = bridge.data_dir().await.expect("provider root");
    let history = |conversation: &str, second: &str| ChatRequest {
        role: "smart".to_owned(),
        messages: vec![
            ChatMessage::User {
                content: "identical first".to_owned(),
            },
            ChatMessage::User {
                content: second.to_owned(),
            },
        ],
        tools: Vec::new(),
        builtin: Vec::new(),
        history: true,
        conversation: Some(conversation.to_owned()),
    };
    for (conversation, second) in [("conv-1", "turn-a"), ("conv-2", "turn-b")] {
        let (chat, _cancel) = control();
        let receiver = bridge
            .chat(history(conversation, second), chat)
            .expect("chat starts");
        collect(receiver, 8, Duration::from_secs(5)).await;
    }
    let id1 = session_of_at(&root, 0);
    let id2 = session_of_at(&root, 1);
    assert!(id1.starts_with("alef-") && id2.starts_with("alef-"));
    // The identical first message must not share a rush session: the id is the identity.
    assert_ne!(id1, id2);
    // The same conversation again reuses its rush id even when the later message differs.
    let (chat, _cancel) = control();
    let receiver = bridge
        .chat(history("conv-1", "turn-a-again"), chat)
        .expect("chat starts");
    collect(receiver, 8, Duration::from_secs(5)).await;
    assert_eq!(session_of_at(&root, 2), id1);
    // Forgetting conv-1 deletes only conv-1's rush session.
    bridge.forget(&history("conv-1", "turn-a"));
    let end = tokio::time::Instant::now() + Duration::from_secs(5);
    loop {
        let log = read_text(&root, "sessions.log");
        if log.contains(&format!("delete {id1}")) {
            assert!(!log.contains(&format!("delete {id2}")));
            break;
        }
        assert!(
            tokio::time::Instant::now() < end,
            "forget did not delete the session: {log}"
        );
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
}

// Mutation guard: reusing a rush session across stateless chats (in `session_id`,
// src/ai/rush.rs) makes this test fail because the two fresh ids would come out equal.
#[tokio::test]
async fn two_stateless_chats_never_share_a_rush_session() {
    let (bridge, _dir) = provider("normal", 8_000, false);
    let root = bridge.data_dir().await.expect("provider root");
    for content in ["stateless one", "stateless two"] {
        let (chat, _cancel) = control();
        let receiver = bridge
            .chat(request("smart", content, false, None), chat)
            .expect("chat starts");
        collect(receiver, 8, Duration::from_secs(5)).await;
    }
    assert_ne!(session_of_at(&root, 0), session_of_at(&root, 1));
}

// Mutation guard: dropping the `.env_clear()` plus filtered re-application on the spawned child
// (in `spawn_process`, src/ai/rush.rs) makes this test fail because env.json would list the
// seeded ALEF_ variable.
#[tokio::test]
async fn the_child_environment_carries_no_alef_variable() {
    std::env::set_var("ALEF_RUSH_BRIDGE_PROBE", "1");
    let (bridge, _dir) = provider("normal", 8_000, true);
    let root = bridge.data_dir().await.expect("provider root");
    let (chat, _cancel) = control();
    let receiver = bridge
        .chat(request("smart", "environment", false, None), chat)
        .expect("chat starts");
    collect(receiver, 8, Duration::from_secs(5)).await;
    let env: serde_json::Value =
        serde_json::from_slice(&read_data(&root, "env.json")).expect("env.json parses");
    assert_eq!(env["alef"], serde_json::json!([]));
    assert!(env["count"].as_u64().expect("a count") > 0);
}

// Mutation guard: replacing the bounded `mpsc::channel(4)` with an unbounded channel (in
// `chat`, src/ai/rush.rs) makes this test fail because the OS pipe and mpsc channel both stall.
#[tokio::test]
async fn the_bounded_channel_stops_the_provider_when_the_page_stops_reading() {
    let (bridge, _dir) = provider("fast-lines", 30_000, false);
    let root = bridge.data_dir().await.expect("provider root");
    let (chat, _cancel) = control();
    let mut receiver = bridge
        .chat(request("smart", "fast", false, None), chat)
        .expect("chat starts");
    let progress = root.join("progress");
    let mut previous = 0;
    let mut stable = 0;
    let end = tokio::time::Instant::now() + Duration::from_secs(3);
    while tokio::time::Instant::now() < end && stable < 3 {
        let current = std::fs::read_to_string(&progress)
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
            .unwrap_or(0);
        if current == previous {
            stable += 1;
        } else {
            stable = 0;
            previous = current;
        }
        tokio::time::sleep(Duration::from_millis(100)).await;
    }
    assert!(stable >= 3, "emulator never stalled at progress {previous}");
    assert!(
        previous < 64,
        "provider consumed all output without a reader"
    );
    let mut texts = 0;
    let end = tokio::time::Instant::now() + Duration::from_secs(10);
    loop {
        let event = tokio::time::timeout_at(end, receiver.recv())
            .await
            .expect("drained stream")
            .expect("provider event");
        if matches!(event, ChatEvent::Text(_)) {
            texts += 1;
        }
        if event == ChatEvent::Completed {
            break;
        }
        assert!(
            !matches!(event, ChatEvent::Failed { .. }),
            "provider failed while resuming"
        );
    }
    assert_eq!(texts, 64);
}

// Mutation guard: removing the cancel/deadline select around the wait-after-EOF (in `run_chat`,
// src/ai/rush.rs) makes this test fail because a child that closed stdout but stayed alive
// would hang the chat past its deadline without ever answering.
#[tokio::test]
async fn a_child_that_closes_stdout_but_stays_alive_is_killed_at_the_deadline() {
    let (bridge, _dir) = provider("eof-hang", 800, false);
    let root = bridge.data_dir().await.expect("provider root");
    let (chat, _cancel) = control();
    let receiver = bridge
        .chat(request("smart", "eof then hang", false, None), chat)
        .expect("chat starts");
    let events = collect(receiver, 8, Duration::from_secs(5)).await;
    assert_eq!(
        events.last(),
        Some(&failed(ErrorCode::Timeout, "the rush run took too long"))
    );
    assert!(events
        .iter()
        .all(|event| matches!(event, ChatEvent::Text(_) | ChatEvent::Failed { .. })));
    // Exactly one Timeout event, and then the channel is closed.
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, ChatEvent::Failed { .. }))
            .count(),
        1
    );
    let pid: u32 = read_text(&root, "run.pid").trim().parse().expect("a pid");
    assert!(pid_gone(pid, Duration::from_secs(5)).await);
}

// Mutation guard: moving the prompt write back into the unguarded `run_chat` body (away from
// its own select-guarded task in src/ai/rush.rs) makes this test fail because the write of a
// 4 MiB prompt to a child that never reads would stall past the cancellation.
#[tokio::test]
async fn a_child_that_never_reads_a_large_prompt_is_cancelled_in_time() {
    let (bridge, _dir) = provider("no-stdin", 30_000, false);
    let root = bridge.data_dir().await.expect("provider root");
    let (chat, cancel) = control();
    let receiver = bridge
        .chat(
            request("smart", &"p".repeat(4 * 1024 * 1024), false, None),
            chat,
        )
        .expect("chat starts");
    let end = tokio::time::Instant::now() + Duration::from_secs(5);
    let pid = loop {
        if let Ok(text) = std::fs::read_to_string(root.join("run.pid")) {
            break text.trim().parse::<u32>().expect("a pid");
        }
        assert!(tokio::time::Instant::now() < end, "no child started");
        tokio::time::sleep(Duration::from_millis(50)).await;
    };
    cancel.cancel();
    assert!(pid_gone(pid, Duration::from_secs(3)).await);
    let events = collect(receiver, 4, Duration::from_millis(500)).await;
    assert!(events.is_empty());
}

// Mutation guard: removing the cancel branch of the probe-phase select at the top of
// `run_chat` (src/ai/rush.rs) makes this test fail because the slow probe would still spawn
// a child (run.pid would appear) or answer with a failure event after the cancel.
#[tokio::test]
async fn a_cancel_during_a_slow_roles_probe_starts_no_child() {
    let (bridge, _dir) = provider("models-slow", 8_000, false);
    let root = bridge.data_dir().await.expect("provider root");
    let (chat, cancel) = control();
    let receiver = bridge
        .chat(request("smart", "slow probe", false, None), chat)
        .expect("chat starts");
    tokio::time::sleep(Duration::from_millis(200)).await;
    cancel.cancel();
    let end = tokio::time::Instant::now() + Duration::from_secs(2);
    while tokio::time::Instant::now() < end {
        assert!(!root.join("run.pid").exists(), "a child started");
        tokio::time::sleep(Duration::from_millis(50)).await;
    }
    let events = collect(receiver, 4, Duration::from_millis(500)).await;
    assert!(events
        .iter()
        .all(|event| !matches!(event, ChatEvent::Failed { .. })));
}
