// SPDX-License-Identifier: MIT OR Apache-2.0
//! The filesystem and the red rights of the AI threat model (docs/stages/m9-ai.md §4, §7): a
//! seeded protected root stays closed through every route, and a command only ever runs behind
//! the red right. Each test names the line of production code whose removal must fail it.
use std::time::Duration;

use alef_core::{
    ids::StreamId,
    protocol::call::Limits,
    protocol::frame::Frame,
    registry::command::Reply,
    security::{
        authority::Authority,
        consent::{Consent, Decision, Right, RiskLevel},
    },
    session::usage,
    AlefError, ErrorCode,
};
use alef_modules::ai;
use serde_json::json;

use super::ai::{
    ai_fixture, ai_fixture_with_manifest, chat_options, drain_text, event, frame, start_chat,
    start_mode, BASE_MANIFEST,
};
use super::ai_threat::{journal_of, threat_manifest};
use super::common::Fixture;

/// `fs.readFile` as bytes, or its error.
async fn read_file(fixture: &Fixture, path: &std::path::Path) -> Result<Vec<u8>, AlefError> {
    match fixture
        .call_reply(
            "fs.readFile",
            json!({ "path": path.to_string_lossy() }),
            None,
        )
        .await?
    {
        Reply::Bytes(bytes) => Ok(bytes.to_vec()),
        other => panic!("expected bytes, got {other:?}"),
    }
}

/// Creates `link` leading to `target`: a symlink on Unix, a junction on Windows. `false` on
/// failure; the caller turns that into a test failure — the link route is part of the
/// guarantee, it never skips.
fn create_link(link: &std::path::Path, target: &std::path::Path) -> bool {
    #[cfg(unix)]
    return std::os::unix::fs::symlink(target, link).is_ok();

    #[cfg(windows)]
    return create_junction(link, target);

    #[cfg(not(any(unix, windows)))]
    {
        let _ = (link, target);
        false
    }
}

/// Windows junction through `mklink`, with a bounded wait: the child is killed at the deadline
/// and the failure is reported instead of hanging the test.
#[cfg(windows)]
fn create_junction(link: &std::path::Path, target: &std::path::Path) -> bool {
    let mut child = match std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(link)
        .arg(target)
        .stdout(std::process::Stdio::null())
        .stderr(std::process::Stdio::null())
        .spawn()
    {
        Ok(child) => child,
        Err(_) => return false,
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(10);
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.success(),
            Ok(None) if std::time::Instant::now() > deadline => {
                let _ = child.kill();
                let _ = child.wait();
                return false;
            }
            Ok(None) => std::thread::sleep(std::time::Duration::from_millis(25)),
            Err(_) => return false,
        }
    }
}

/// Mutation guard: the journal write gates every run (src/ai/launch.rs, the `record` before the
/// builtin executes and before the tool_call frame reaches the page), and the result of a tool
/// whose right the user took back is refused (src/ai/mod.rs `ai.tools.result`, the permission
/// re-check before `pending.remove`).
#[tokio::test]
async fn tool_consent_change_and_journal_gate_are_enforced() {
    // The user takes the tool right back while a call is pending: the answer is refused and the
    // run never completes.
    let fixture = ai_fixture("threat-consent-change", Consent::allow_all()).await;
    let registered = fixture
        .call("ai.tools.handle", json!({"name": "summarizeSelection"}))
        .await
        .unwrap();
    let (chat, stream, mut reader) =
        start_chat(&fixture, chat_options(&["summarizeSelection"])).await;
    frame(&mut reader).await;
    frame(&mut reader).await;
    let payload = event(&fixture, "ai.tool_call").await;
    assert_eq!(payload["callId"], json!("call-0"));
    let mut taken_back = Consent::allow_all();
    taken_back.set(
        Right::scoped("ai.tool", "summarizeSelection"),
        Decision::Deny,
    );
    assert!(
        fixture.permissions().narrow(&taken_back),
        "the deny narrowed the right"
    );
    let answer = json!({
        "chat": chat,
        "callId": "call-0",
        "name": "summarizeSelection",
        "result": {"summary": "ran without the right"},
    });
    assert_eq!(
        fixture
            .call("ai.tools.result", answer)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Closed,
        "the refused answer was accepted"
    );
    fixture
        .session()
        .streams()
        .close(StreamId(stream))
        .expect("the stream closes");
    let mut gone = false;
    for _ in 0..250 {
        if !ai::chat_is_pending(&fixture.session(), chat) {
            gone = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(
        gone,
        "the chat was never torn down after the refused answer"
    );
    let entries = journal_of(&fixture.context.ai_home);
    assert!(
        entries
            .iter()
            .all(|entry| entry.outcome.as_deref() != Some("ok")),
        "the refused answer completed a run: {entries:?}"
    );
    let _ = fixture
        .call(
            "ai.tools.unhandle",
            json!({"registration": registered["registration"]}),
        )
        .await;

    // The journal gate for a builtin: an unavailable journal leaves the effect zero.
    let scratch = tempfile::tempdir().unwrap();
    let allowed = scratch.path().join("allowed");
    std::fs::create_dir(&allowed).unwrap();
    let sentinel = allowed.join("sentinel.txt");
    std::fs::write(&sentinel, "builtin sentinel").unwrap();
    let scope = format!("{}/*", allowed.display()).replace('\\', "/");
    let manifest = threat_manifest("smart", None, Some(&scope));
    let root = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .join("alef-modules-paths");
    let fixture = Fixture::new_sharing_authority(
        &root,
        Some(&manifest),
        std::sync::Arc::new(alef_modules::MemorySecrets::default()),
        Authority::SignedPackage {
            fingerprint: "threat-journal".to_owned(),
        },
    )
    .await
    .with_consent(Consent::allow_all());
    std::fs::create_dir_all(&fixture.context.ai_home).unwrap();
    std::fs::create_dir(fixture.context.ai_home.join("journal.log")).unwrap();
    let (_, mut reader) = start_mode(
        &fixture,
        "ai.request",
        json!({
            "role": "smart",
            "messages": [{"role": "user", "content": format!("{{\"path\":{}}}", serde_json::to_string(&sentinel.display().to_string()).unwrap())}],
            "builtin": ["fs.read"],
        }),
    )
    .await;
    frame(&mut reader).await;
    assert!(
        matches!(frame(&mut reader).await, Frame::Error(error) if error.code == ErrorCode::Internal)
    );
    let mut after = String::new();
    for _ in 0..8 {
        match tokio::time::timeout(Duration::from_millis(500), reader.next_frame()).await {
            Err(_) | Ok(None) => break,
            Ok(Some(Frame::Json(value))) => after.push_str(&value.to_string()),
            Ok(Some(Frame::End)) => break,
            Ok(Some(Frame::Error(error))) => panic!("a second terminal frame: {error:?}"),
            Ok(Some(Frame::Binary(bytes))) => {
                panic!("unexpected binary frame: {} bytes", bytes.len())
            }
        }
    }
    assert!(
        !after.contains("builtin sentinel"),
        "the gated builtin still read the file: {after}"
    );
    // The journal path is a directory: it cannot be read as a journal, and it must stay untouched.
    assert!(
        fixture.context.ai_home.join("journal.log").is_dir(),
        "the refused builtin run replaced the blocked journal"
    );

    // The same gate for an application tool: no journal entry, no tool_call event, no effect.
    let fixture = ai_fixture("threat-journal-tool", Consent::allow_all()).await;
    std::fs::create_dir_all(&fixture.context.ai_home).unwrap();
    std::fs::create_dir(fixture.context.ai_home.join("journal.log")).unwrap();
    fixture
        .call("ai.tools.handle", json!({"name": "summarizeSelection"}))
        .await
        .unwrap();
    let (_, _stream, mut reader) =
        start_chat(&fixture, chat_options(&["summarizeSelection"])).await;
    frame(&mut reader).await;
    assert!(
        matches!(frame(&mut reader).await, Frame::Error(error) if error.code == ErrorCode::Internal),
        "the chat did not fail on the journal gate"
    );
    assert_eq!(
        fixture
            .host
            .events
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, name, _)| name == "ai.tool_call")
            .count(),
        0,
        "the tool call was announced before the journal entry"
    );
    assert!(
        fixture.context.ai_home.join("journal.log").is_dir(),
        "the broken journal was replaced by a recorded entry"
    );
}

/// Mutation guard: every terminal outcome goes through the single `abandon` and the one
/// `writer.error` or one `finish_turn` of the producer loop (src/ai/launch.rs `produce`), and
/// `StreamHub::close` terminates the reader with exactly one error frame (session/streams);
/// removing the closed-stream branch, the return after it, or double-terminating the writer is
/// caught here. The §6 deadlines themselves are NOT proven: no seam shortens them.
#[tokio::test]
async fn credit_cancel_and_terminal_events_are_bounded() {
    let fixture = ai_fixture("threat-terminal", Consent::allow_all()).await;
    // A chat held at the provider gate is cancelled with its stream: the teardown is bounded and
    // the reader sees exactly one error of code Closed, then silence forever.
    let gate = fixture.ai.hold_next_chat();
    let (chat, stream, mut reader) = start_chat(&fixture, chat_options(&[])).await;
    assert_eq!(fixture.ai.chats_started(), 1);
    fixture
        .session()
        .streams()
        .close(StreamId(stream))
        .expect("the stream closes");
    drop(gate);
    let mut gone = false;
    for _ in 0..250 {
        if !ai::chat_is_pending(&fixture.session(), chat) {
            gone = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(gone, "the gated chat was never torn down");
    let terminal = tokio::time::timeout(Duration::from_millis(500), reader.next_frame())
        .await
        .expect("the wait ends");
    assert!(
        matches!(&terminal, Some(Frame::Error(error)) if error.code == ErrorCode::Closed),
        "the closed stream did not answer with exactly one Closed error: {terminal:?}"
    );
    for _ in 0..2 {
        let next = tokio::time::timeout(Duration::from_millis(300), reader.next_frame())
            .await
            .expect("the wait ends");
        assert_eq!(next, None, "a dead chat produced another frame");
    }

    // The control: the gate released, the chat ends with exactly ONE terminal outcome (the end
    // part) and nothing follows it.
    let (_chat, _stream, mut reader) = start_chat(&fixture, chat_options(&[])).await;
    let (mut texts, mut usages, mut ends) = (0, 0, 0);
    loop {
        match tokio::time::timeout(Duration::from_secs(5), reader.next_frame())
            .await
            .expect("a frame in time")
        {
            Some(Frame::Json(value)) => match value["type"].as_str() {
                Some("text") => texts += 1,
                Some("usage") => usages += 1,
                Some("end") => ends += 1,
                other => panic!("unexpected part {other:?}: {value}"),
            },
            Some(Frame::End) => break,
            Some(Frame::Error(error)) => panic!("unexpected stream error: {error:?}"),
            Some(Frame::Binary(bytes)) => panic!("unexpected binary frame: {} bytes", bytes.len()),
            None => panic!("the stream ended without its End frame"),
        }
    }
    assert_eq!((texts, usages, ends), (1, 1, 1));
    for _ in 0..2 {
        let next = tokio::time::timeout(Duration::from_millis(300), reader.next_frame())
            .await
            .expect("the wait ends");
        assert_eq!(next, None, "nothing follows the end");
    }
}

/// An 8-tool manifest (the pending-call budget is eight, and the fake raises every call before it
/// waits for results): the chat produces more frame bytes than the small window of the fixture
/// below carries.
fn eight_tools_manifest() -> String {
    let tools = (0..8)
        .map(|index| {
            format!(
                "            {{\n                name: tool{index:02}\n                description: Tool {index:02}\n                schema: {{\n                    type: object\n                    properties: {{\n                        text: {{\n                            type: string\n                        }}\n                    }}\n                    required: [text]\n                    additionalProperties: false\n                }}\n            }}\n"
            )
        })
        .collect::<String>();
    BASE_MANIFEST.replace(
        "    app: {",
        &format!("    ai: {{\n        roles: [smart]\n        tools: [\n{tools}        ]\n    }}\n    app: {{"),
    )
}

/// Mutation guard: the outgoing stream runs on credit (`CreditGate`, alef-core session/streams;
/// the session window comes from `Limits.stream_window`): without acks the producer stops at the
/// window, acking resumes it. Removing the gate or the ack granting floods the reader instead.
#[tokio::test]
async fn the_stream_credit_window_stops_and_resumes_the_producer() {
    let limits = Limits {
        stream_window: 4096,
        ..Limits::default()
    };
    let manifest = eight_tools_manifest();
    let fixture = Fixture::new_limited(Some(&manifest), limits).await;
    for index in 0..8 {
        fixture
            .call(
                "ai.tools.handle",
                json!({ "name": format!("tool{index:02}") }),
            )
            .await
            .unwrap();
    }
    let names: Vec<String> = (0..8).map(|index| format!("tool{index:02}")).collect();
    // Every tool call carries about a kilobyte of arguments: at most three fit into the window.
    let arguments = json!({ "input": "x".repeat(1024) }).to_string();
    let (chat, stream, mut reader) = start_chat(
        &fixture,
        json!({
            "role": "smart",
            "messages": [{"role": "user", "content": arguments}],
            "tools": names,
        }),
    )
    .await;
    // No acks: the producer stalls inside the window. The reader must not see more frames than
    // fit, and the outstanding bytes never exceed the window.
    let mut received = 0;
    let mut calls = 0;
    let mut texts = String::new();
    loop {
        match tokio::time::timeout(Duration::from_millis(700), reader.next_frame()).await {
            Err(_) => break,
            Ok(Some(Frame::Json(value))) => {
                match value["type"].as_str() {
                    Some("text") => texts.push_str(value["text"].as_str().expect("text payload")),
                    Some("tool_call") => calls += 1,
                    other => panic!("unexpected early part {other:?}: {value}"),
                }
                received += 1;
            }
            Ok(other) => panic!("the chat moved without credit: {other:?}"),
        }
    }
    // The text and eight calls of about a kilobyte each do not fit into 4096 bytes: a producer
    // that ignored the window would have delivered all nine frames.
    assert!(
        (1..=5).contains(&received),
        "the producer delivered {received} frames without acks: the window did not stop it"
    );
    let outstanding = fixture
        .session()
        .streams()
        .outstanding(StreamId(stream))
        .expect("outstanding");
    assert!(
        outstanding <= 4096,
        "the window was exceeded: {outstanding}"
    );
    tokio::time::sleep(Duration::from_millis(200)).await;
    let after_pause = fixture
        .session()
        .streams()
        .outstanding(StreamId(stream))
        .expect("outstanding");
    assert_eq!(
        outstanding, after_pause,
        "the producer did not stay stopped"
    );
    // Acking resumes the producer: the page acks what it consumed, and every call arrives.
    fixture
        .session()
        .streams()
        .ack(StreamId(stream), 1 << 20)
        .expect("the ack");
    while calls < 8 {
        match tokio::time::timeout(Duration::from_secs(5), reader.next_frame())
            .await
            .expect("a frame in time")
        {
            Some(Frame::Json(value)) => match value["type"].as_str() {
                Some("tool_call") => calls += 1,
                Some("text") => texts.push_str(value["text"].as_str().expect("text payload")),
                Some("usage") | Some("end") => {}
                other => panic!("unexpected part {other:?}: {value}"),
            },
            Some(Frame::End) => break,
            Some(Frame::Error(error)) => panic!("unexpected stream error: {error:?}"),
            Some(Frame::Binary(bytes)) => panic!("unexpected binary frame: {} bytes", bytes.len()),
            None => panic!("the stream ended without its End frame"),
        }
        fixture
            .session()
            .streams()
            .ack(StreamId(stream), 1 << 20)
            .expect("the ack");
    }
    assert_eq!(calls, 8, "not every call arrived after the ack");
    for index in 0..8 {
        fixture
            .call(
                "ai.tools.result",
                json!({
                    "chat": chat,
                    "callId": format!("call-{index}"),
                    "name": format!("tool{index:02}"),
                    "result": {"summary": "ok"},
                }),
            )
            .await
            .unwrap();
    }
    // The remainder: the fake reports what it saw and completes exactly once.
    let mut ends = 0;
    loop {
        match tokio::time::timeout(Duration::from_secs(5), reader.next_frame())
            .await
            .expect("a frame in time")
        {
            Some(Frame::Json(value)) => match value["type"].as_str() {
                Some("text") => texts.push_str(value["text"].as_str().expect("text payload")),
                Some("usage") => {}
                Some("end") => ends += 1,
                other => panic!("unexpected part {other:?}: {value}"),
            },
            Some(Frame::End) => break,
            Some(Frame::Error(error)) => panic!("unexpected stream error: {error:?}"),
            Some(Frame::Binary(bytes)) => panic!("unexpected binary frame: {} bytes", bytes.len()),
            None => panic!("the stream ended without its End frame"),
        }
        fixture
            .session()
            .streams()
            .ack(StreamId(stream), 1 << 20)
            .expect("the ack");
    }
    assert!(texts.contains("Saw 8 tool result(s)"), "{texts}");
    assert_eq!(ends, 1, "exactly one end outcome");
    let after = tokio::time::timeout(Duration::from_millis(300), reader.next_frame())
        .await
        .expect("the wait ends");
    assert_eq!(after, None, "nothing follows the end");
}

/// Mutation guard: `PermissionSet::authorize_at` refuses every canonical path whose components
/// reach a protected root (src/security/permissions.rs, the `is_protected` check), and
/// `authorize_ai_builtin_path` does the same for AI builtins (src/security/given/builtin.rs);
/// deleting either check lets the granted scope read the seeded root — the scope below
/// CONTAINS the protected root, so without the protection the probes are plain scope matches
/// and would pass. A granted `cli.exec` of a same-user reader bypasses this boundary by design
/// (the red right): that residual risk is documented, not tested here, hence the name.
#[tokio::test]
async fn app_cannot_read_rush_files_through_fs_or_builtin_rights() {
    let scratch = tempfile::tempdir().unwrap();
    let rush_root = scratch.path().join("rush-config");
    std::fs::create_dir_all(rush_root.join("keys")).unwrap();
    let rush_file = rush_root.join("keys").join("db.json");
    std::fs::write(&rush_file, "rush-secret-sentinel").unwrap();
    let allowed = scratch.path().join("allowed");
    std::fs::create_dir_all(allowed.join("project").join(".rush")).unwrap();
    std::fs::write(allowed.join("normal.txt"), "normal-content").unwrap();
    std::fs::write(allowed.join("project").join("src.txt"), "source-content").unwrap();
    std::fs::write(
        allowed.join("project").join(".rush").join("db.json"),
        "dotrush-sentinel",
    )
    .unwrap();

    // The grant CONTAINS the protected root: a denial of the root can only come from the
    // protection, never from the scope.
    let scope = format!("{}/**", scratch.path().display()).replace('\\', "/");
    let manifest = BASE_MANIFEST
        .replace('\r', "")
        .replace("        read: []", &format!("        read: [ {scope} ]"))
        .replace(
            "    app: {",
            &format!(
                "    ai: {{\n        roles: [smart]\n        tools: []\n        builtin: [\n            {{\n                name: fs.read\n                scopes: [{scope}]\n            }}\n        ]\n    }}\n    app: {{"
            ),
        );
    let root = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .join("alef-modules-paths");
    let fixture = Fixture::new_sharing_authority(
        &root,
        Some(&manifest),
        std::sync::Arc::new(alef_modules::MemorySecrets::default()),
        Authority::SignedPackage {
            fingerprint: "threat-roots".to_owned(),
        },
    )
    .await
    .with_consent(Consent::allow_all())
    .with_protected_folder(&rush_root);

    // The fs right: inside the seeded root refused although the scope grants it; the neighbour
    // normal file is allowed (the control that the fixture reaches the route at all).
    let error = read_file(&fixture, &rush_file).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::PermissionDenied);
    assert_eq!(
        read_file(&fixture, &allowed.join("normal.txt"))
            .await
            .unwrap(),
        b"normal-content"
    );
    // A `.rush` folder inside the grant is refused by name; its sibling is allowed.
    let error = read_file(
        &fixture,
        &allowed.join("project").join(".rush").join("db.json"),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::PermissionDenied);
    assert_eq!(
        read_file(&fixture, &allowed.join("project").join("src.txt"))
            .await
            .unwrap(),
        b"source-content"
    );

    // The builtin: the path of the model is canonicalized and checked against the seeded root.
    let builtin_read = |path: &std::path::Path| {
        start_mode(
            &fixture,
            "ai.request",
            json!({
                "role": "smart",
                "messages": [{"role": "user", "content": format!("{{\"path\":{}}}", serde_json::to_string(&path.display().to_string()).unwrap())}],
                "builtin": ["fs.read"],
            }),
        )
    };
    let (_, mut reader) = builtin_read(&rush_file).await;
    frame(&mut reader).await;
    assert!(
        matches!(frame(&mut reader).await, Frame::Error(error) if error.code == ErrorCode::PermissionDenied)
    );
    // The control: the same builtin reads the allowed file.
    let (_, mut reader) = builtin_read(&allowed.join("normal.txt")).await;
    frame(&mut reader).await;
    assert!(drain_text(&mut reader).await.contains("normal-content"));

    // A link inside the grant that leads into the seeded root is followed into the refusal. The
    // link must be creatable: the route is part of the guarantee, it never skips.
    assert!(
        create_link(&allowed.join("alias"), &rush_root),
        "the junction/symlink into the rush root could not be created"
    );
    let error = read_file(
        &fixture,
        &allowed.join("alias").join("keys").join("db.json"),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::PermissionDenied);
    let (_, mut reader) = builtin_read(&allowed.join("alias").join("keys").join("db.json")).await;
    frame(&mut reader).await;
    assert!(
        matches!(frame(&mut reader).await, Frame::Error(error) if error.code == ErrorCode::PermissionDenied)
    );

    // cli.exec of a file reader is refused without the red right (the manifest declares none).
    let reader = format!(
        r#"node -e "process.stdout.write(require('fs').readFileSync(process.argv[1],'utf8'))" "{}""#,
        rush_file.display().to_string().replace('\\', "/")
    );
    let error = fixture
        .call("cli.exec", json!({"commandLine": reader}))
        .await
        .unwrap_err();
    assert_eq!(
        error.code,
        ErrorCode::PermissionDenied,
        "the reader ran without the red right"
    );
}

/// Mutation guard: the execution builtins never run — `builtin::offered` offers only what the
/// manifest declares and the user allowed, and `builtin::execute` refuses `exec`/`bash` outright
/// (src/ai/builtin.rs, the `execution builtin is unavailable` branch); letting either pass hands
/// the model a command execution.
#[tokio::test]
async fn a_red_right_is_the_only_way_to_run_commands() {
    // The risk model asks for the execution rights as red: the window shows them as red and
    // keeps the start disabled until they are confirmed. The window flow itself is covered by
    // the `ai` scenario's window-consent mode; the runtime cannot tell confirmed from not.
    assert_eq!(Right::plain("ai.builtin.exec").level(), RiskLevel::Red);
    assert_eq!(Right::plain("ai.builtin.bash").level(), RiskLevel::Red);
    assert_eq!(Right::scoped("cli.exec", "node").level(), RiskLevel::Red);
    assert_eq!(
        Right::scoped("fs.read", "$TEMP/**").level(),
        RiskLevel::Normal
    );

    // Declared and allowed: the offer reaches the provider (the control that the path is live),
    // and the runtime alone refuses to run it; the user's journal records the attempt.
    let fixture = ai_fixture_with_manifest("threat-red-allowed", Consent::allow_all(), true).await;
    let (_, mut reader) = start_mode(
        &fixture,
        "ai.request",
        json!({
            "role": "smart",
            "messages": [{"role": "user", "content": "hello"}],
            "builtin": ["exec"],
        }),
    )
    .await;
    frame(&mut reader).await;
    assert!(
        matches!(frame(&mut reader).await, Frame::Error(error) if error.code == ErrorCode::NotAvailable)
    );
    let entries = journal_of(&fixture.context.ai_home);
    let starts = entries
        .iter()
        .filter(|entry| {
            entry.kind == usage::Kind::Builtin
                && entry.name == "exec"
                && entry.event == usage::Event::Start
        })
        .count();
    let errors = entries
        .iter()
        .filter(|entry| {
            entry.kind == usage::Kind::Builtin
                && entry.name == "exec"
                && entry.event == usage::Event::End
                && entry.outcome.as_deref() == Some("error")
        })
        .count();
    assert_eq!(
        (starts, errors),
        (1, 1),
        "the refused run is journalled exactly once: {entries:?}"
    );

    // Denied: refused before the provider, unlike the allowed control above.
    let mut denied = Consent::allow_all();
    denied.set(Right::plain("ai.builtin.exec"), Decision::Deny);
    let fixture = ai_fixture_with_manifest("threat-red-denied", denied, true).await;
    let before = fixture.ai.chats_started();
    let error = fixture
        .call(
            "ai.request",
            json!({
                "role": "smart",
                "messages": [{"role": "user", "content": "hello"}],
                "builtin": ["exec"],
            }),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::PermissionDenied);
    assert_eq!(fixture.ai.chats_started(), before);
    // `bash` is not even declared: the same refusal.
    let fixture = ai_fixture_with_manifest("threat-red-bash", Consent::allow_all(), false).await;
    let before = fixture.ai.chats_started();
    let error = fixture
        .call(
            "ai.request",
            json!({
                "role": "smart",
                "messages": [{"role": "user", "content": "hello"}],
                "builtin": ["bash"],
            }),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::PermissionDenied);
    assert_eq!(fixture.ai.chats_started(), before);
}
