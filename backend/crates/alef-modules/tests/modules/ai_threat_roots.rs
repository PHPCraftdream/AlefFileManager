// SPDX-License-Identifier: MIT OR Apache-2.0
//! The filesystem and the red rights of the AI threat model (docs/stages/m9-ai.md §4, §7): a
//! seeded protected root stays closed through every route, and a command only ever runs behind
//! the red right. Each test names the line of production code whose removal must fail it.
use std::time::Duration;

use alef_core::{
    ids::StreamId,
    protocol::frame::Frame,
    registry::command::Reply,
    security::{
        authority::Authority,
        consent::{Consent, Decision, Right},
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

/// Creates `link` leading to `target`: a symlink on Unix, a junction on Windows. `false` when the
/// machine refuses links: the route stays untested instead of silently passing.
fn create_link(link: &std::path::Path, target: &std::path::Path) -> bool {
    #[cfg(unix)]
    return std::os::unix::fs::symlink(target, link).is_ok();
    #[cfg(windows)]
    return std::process::Command::new("cmd")
        .args(["/C", "mklink", "/J"])
        .arg(link)
        .arg(target)
        .output()
        .is_ok_and(|output| output.status.success());
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (link, target);
        false
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
}

/// Mutation guard: every terminal outcome goes through the single `abandon` and the one
/// `writer.error` or one `finish_turn` of the producer loop (src/ai/launch.rs `produce`);
/// removing the closed-stream branch or the return after it leaves the chat pending or emits a
/// second outcome.
#[tokio::test]
async fn credit_deadlines_cancel_and_terminal_events_are_bounded() {
    let fixture = ai_fixture("threat-terminal", Consent::allow_all()).await;
    // A chat held at the provider gate is cancelled with its stream: the teardown is bounded and
    // the stream slot is freed.
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
    for _ in 0..2 {
        let next = tokio::time::timeout(Duration::from_millis(300), reader.next_frame())
            .await
            .expect("the wait ends");
        assert_eq!(next, None, "a dead chat produced a frame");
    }
    // The control: the gate released, the chat completes exactly once and then stays quiet.
    let gate = fixture.ai.hold_next_chat();
    let (_chat, _stream, mut reader) = start_chat(&fixture, chat_options(&[])).await;
    gate.release();
    assert_eq!(drain_text(&mut reader).await, "Thinking");
    let next = tokio::time::timeout(Duration::from_millis(300), reader.next_frame())
        .await
        .expect("the wait ends");
    assert_eq!(next, None, "nothing follows the end");
}

/// Mutation guard: `PermissionSet::authorize_at` refuses every canonical path whose components
/// reach a protected root (src/security/permissions.rs, the `is_protected` check), and
/// `authorize_ai_builtin_path` does the same for AI builtins (src/security/given/builtin.rs);
/// deleting either check lets a scope that names a broad parent read the seeded root.
#[tokio::test]
async fn app_cannot_read_rush_files_through_fs_cli_shell_or_builtin() {
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

    let scope = format!("{}/**", allowed.display()).replace('\\', "/");
    let manifest = BASE_MANIFEST
        .replace('\r', "")
        .replace("        read: []", &format!("        read: [ {scope} ]"))
        .replace("        exec: []", "        exec: [ node ]")
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

    // The fs right: inside the seeded root refused, the sibling normal file allowed (the control).
    let error = read_file(&fixture, &rush_file).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::PermissionDenied);
    assert_eq!(
        read_file(&fixture, &allowed.join("normal.txt"))
            .await
            .unwrap(),
        b"normal-content"
    );
    // A `.rush` folder inside the granted scope is refused by name; its sibling is allowed.
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

    // A link inside the scope that leads into the seeded root is followed into the refusal.
    if create_link(&allowed.join("alias"), &rush_root) {
        let error = read_file(
            &fixture,
            &allowed.join("alias").join("keys").join("db.json"),
        )
        .await
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::PermissionDenied);
        let (_, mut reader) =
            builtin_read(&allowed.join("alias").join("keys").join("db.json")).await;
        frame(&mut reader).await;
        assert!(
            matches!(frame(&mut reader).await, Frame::Error(error) if error.code == ErrorCode::PermissionDenied)
        );
    } else {
        println!("link creation unavailable: the link route is untested on this machine");
    }

    // cli.exec of a file reader without the red right: the reader never runs.
    let bare = Fixture::new_sharing_authority(
        &root,
        Some(BASE_MANIFEST),
        std::sync::Arc::new(alef_modules::MemorySecrets::default()),
        Authority::SignedPackage {
            fingerprint: "threat-roots-bare".to_owned(),
        },
    )
    .await
    .with_consent(Consent::allow_all());
    let reader = format!(
        r#"node -e "process.stdout.write(require('fs').readFileSync(process.argv[1],'utf8'))" "{}""#,
        rush_file.display().to_string().replace('\\', "/")
    );
    let error = bare
        .call("cli.exec", json!({"commandLine": reader}))
        .await
        .unwrap_err();
    assert_eq!(
        error.code,
        ErrorCode::PermissionDenied,
        "the reader ran without the red right"
    );
    // The control: the same reader with `node` declared reads the normal file.
    let control = format!(
        r#"node -e "process.stdout.write(require('fs').readFileSync(process.argv[1],'utf8'))" "{}""#,
        allowed
            .join("normal.txt")
            .display()
            .to_string()
            .replace('\\', "/")
    );
    let reply = fixture
        .call("cli.exec", json!({"commandLine": control}))
        .await
        .unwrap();
    assert_eq!(reply["stdout"], "normal-content");
}

/// Mutation guard: the execution builtins never run — `builtin::offered` offers only what the
/// manifest declares and the user allowed, and `builtin::execute` refuses `exec`/`bash` outright
/// (src/ai/builtin.rs, the `execution builtin is unavailable` branch); letting either pass hands
/// the model a command execution.
#[tokio::test]
async fn a_red_right_is_the_only_way_to_run_commands() {
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
