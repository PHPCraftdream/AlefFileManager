// SPDX-License-Identifier: MIT OR Apache-2.0
//! The per-application quota and the user journal of the `ai` module (task #165): the daily
//! limits refuse the attempt before the provider, and the journal records every tool run.

use alef_core::{
    ids::StreamId, protocol::frame::Frame, security::consent::Consent, session::usage, ErrorCode,
};
use serde_json::json;

use super::ai::{
    ai_fixture, chat_options, drain_text, frame, start_chat, start_mode, BASE_MANIFEST,
};
use super::common::Fixture;

/// The daily limits the tests set BEFORE the first chat, through the same store the module uses.
fn set_limits(fixture: &Fixture, requests: Option<u64>, tokens: Option<u64>) {
    usage::Store::new(fixture.context.ai_home.clone())
        .set_limits(&usage::Limits { requests, tokens })
        .unwrap();
}

#[tokio::test]
async fn a_chat_over_the_request_limit_is_quota_exceeded_and_starts_nothing() {
    let fixture = ai_fixture("quota-requests", Consent::allow_all()).await;
    set_limits(&fixture, Some(1), None);
    let (_chat, _stream, mut reader) = start_chat(&fixture, chat_options(&[])).await;
    assert_eq!(drain_text(&mut reader).await, "Thinking");
    let error = fixture
        .call("ai.chat", chat_options(&[]))
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::QuotaExceeded);
    // The refused attempt never reached the provider.
    assert_eq!(fixture.ai.chats_started(), 1);
}

#[tokio::test]
async fn a_chat_over_the_token_limit_never_reaches_the_provider() {
    let fixture = ai_fixture("quota-tokens", Consent::allow_all()).await;
    // The fake names a 4096-token bound: ten tokens a day refuse the very first chat.
    set_limits(&fixture, None, Some(10));
    let error = fixture
        .call("ai.chat", chat_options(&[]))
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::QuotaExceeded);
    assert_eq!(fixture.ai.chats_started(), 0);
}

/// The parsed journal entries of the fixture, oldest first.
fn journal(fixture: &Fixture) -> Vec<usage::Entry> {
    usage::Journal::read(&fixture.context.ai_home)
        .unwrap()
        .iter()
        .map(|line| serde_json::from_str(line).unwrap())
        .collect()
}

#[tokio::test]
async fn the_journal_records_a_tool_run_before_and_after() {
    use usage::{Event, Kind};
    let fixture = ai_fixture("journal-tool", Consent::allow_all()).await;
    let registered = fixture
        .call("ai.tools.handle", json!({"name": "summarizeSelection"}))
        .await
        .unwrap();
    let (chat, _stream, mut reader) =
        start_chat(&fixture, chat_options(&["summarizeSelection"])).await;
    frame(&mut reader).await;
    frame(&mut reader).await;
    fixture
        .call(
            "ai.tools.result",
            json!({
                "chat": chat,
                "callId": "call-0",
                "name": "summarizeSelection",
                "result": {"summary": "ok"},
            }),
        )
        .await
        .unwrap();
    drain_text(&mut reader).await;
    let entries = journal(&fixture);
    let start = entries
        .iter()
        .position(|entry| {
            entry.kind == Kind::Tool
                && entry.name == "summarizeSelection"
                && entry.role == "smart"
                && entry.event == Event::Start
        })
        .expect("a start entry for the tool run");
    let end = &entries[start + 1];
    assert_eq!(end.kind, Kind::Tool);
    assert_eq!(end.name, "summarizeSelection");
    assert_eq!(end.event, Event::End);
    assert_eq!(end.outcome.as_deref(), Some("ok"));
    // The registration of this test must not leak into the next one.
    let _ = fixture
        .call(
            "ai.tools.unhandle",
            json!({"registration": registered["registration"]}),
        )
        .await;
}

#[tokio::test]
async fn the_journal_records_a_builtin_run() {
    use usage::{Event, Kind};
    let scratch = tempfile::tempdir().unwrap();
    let allowed = scratch.path().join("allowed");
    std::fs::create_dir(&allowed).unwrap();
    let sentinel = allowed.join("sentinel.txt");
    std::fs::write(&sentinel, "builtin sentinel").unwrap();
    let scope = format!("{}/*", allowed.display()).replace('\\', "/");
    let manifest = BASE_MANIFEST.replace(
        "    app: {",
        &format!(
            "    ai: {{\n        roles: [smart, fast]\n        tools: []\n        builtin: [\n            {{\n                name: fs.read\n                scopes: [{scope}]\n            }}\n        ]\n    }}\n    app: {{"
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
        alef_core::security::authority::Authority::SignedPackage {
            fingerprint: "journal-builtin".to_owned(),
        },
    )
    .await
    .with_consent(Consent::allow_all());
    let (_, mut reader) = start_mode(
        &fixture,
        "ai.request",
        json!({
            "role": "smart",
            "messages": [{"role": "user", "content": format!("{{\"path\":{}}}", serde_json::to_string(&sentinel.to_string_lossy()).unwrap())}],
            "builtin": ["fs.read"],
        }),
    )
    .await;
    let response = drain_text(&mut reader).await;
    assert!(response.contains("builtin sentinel"));
    let entries = journal(&fixture);
    let start = entries
        .iter()
        .position(|entry| {
            entry.kind == Kind::Builtin && entry.name == "fs.read" && entry.event == Event::Start
        })
        .expect("a start entry for the builtin run");
    let end = &entries[start + 1];
    assert_eq!(end.kind, Kind::Builtin);
    assert_eq!(end.name, "fs.read");
    assert_eq!(end.event, Event::End);
    assert_eq!(end.outcome.as_deref(), Some("ok"));
}

#[tokio::test]
async fn a_tool_whose_journal_cannot_be_written_never_reaches_the_page() {
    let fixture = ai_fixture("journal-denied", Consent::allow_all()).await;
    // A directory named `journal.log` makes every append fail on every platform.
    std::fs::create_dir_all(&fixture.context.ai_home).unwrap();
    std::fs::create_dir(fixture.context.ai_home.join("journal.log")).unwrap();
    fixture
        .call("ai.tools.handle", json!({"name": "summarizeSelection"}))
        .await
        .unwrap();
    let (_chat, _stream, mut reader) =
        start_chat(&fixture, chat_options(&["summarizeSelection"])).await;
    let mut failed = None;
    for _ in 0..32 {
        match frame(&mut reader).await {
            Frame::Json(_) => {}
            Frame::End => panic!("the chat completed without its journal entry"),
            Frame::Binary(bytes) => panic!("unexpected binary frame: {} bytes", bytes.len()),
            Frame::Error(error) => {
                failed = Some(error);
                break;
            }
        }
    }
    let error = failed.expect("the chat failed on the journal");
    assert_eq!(error.code, ErrorCode::Internal);
    // The gate sits between the provider and the page: no tool call event ever went out.
    assert!(fixture
        .host
        .events
        .lock()
        .unwrap()
        .iter()
        .all(|(_, name, _)| name != "ai.tool_call"));
}

#[tokio::test]
async fn a_cancelled_tool_run_journals_the_cancelled_outcome() {
    use usage::{Event, Kind};
    let fixture = ai_fixture("journal-cancelled", Consent::allow_all()).await;
    fixture
        .call("ai.tools.handle", json!({"name": "summarizeSelection"}))
        .await
        .unwrap();
    let (_chat, stream, mut reader) =
        start_chat(&fixture, chat_options(&["summarizeSelection"])).await;
    // Wait for the tool_call frame, then close the stream: the chat dies and the
    // pending call must land on the journal as cancelled.
    for _ in 0..32 {
        match frame(&mut reader).await {
            Frame::Json(value) if value["type"] == "tool_call" => break,
            Frame::End | Frame::Error(_) => panic!("the chat ended before the tool call"),
            _ => {}
        }
    }
    drop(reader);
    fixture
        .session()
        .streams()
        .close(StreamId(stream))
        .expect("the stream closes");
    // Bounded poll: the journal entry is written by the producer after it notices the close.
    let mut entries = Vec::new();
    for _ in 0..200 {
        entries = journal(&fixture);
        if entries.iter().any(|entry| {
            entry.kind == Kind::Tool
                && entry.name == "summarizeSelection"
                && entry.event == Event::End
                && entry.outcome.as_deref() == Some("cancelled")
        }) {
            break;
        }
        tokio::time::sleep(std::time::Duration::from_millis(25)).await;
    }
    assert!(
        entries.iter().any(|entry| {
            entry.kind == Kind::Tool
                && entry.name == "summarizeSelection"
                && entry.event == Event::End
                && entry.outcome.as_deref() == Some("cancelled")
        }),
        "no cancelled outcome in the journal: {entries:?}"
    );
}

#[tokio::test]
async fn a_session_send_reserves_from_the_same_quota() {
    let fixture = ai_fixture("quota-session", Consent::allow_all()).await;
    set_limits(&fixture, Some(1), None);
    let opened = fixture
        .call("ai.session.open", json!({"role": "smart"}))
        .await
        .unwrap();
    let conversation = opened["conversation"].as_u64().unwrap();
    let send = json!({
        "conversation": conversation,
        "messages": [{"role": "user", "content": "alpha"}],
        "tools": [],
    });
    let (_, mut reader) = start_mode(&fixture, "ai.session.send", send.clone()).await;
    drain_text(&mut reader).await;
    // The second send shares the same daily quota of the application.
    let error = fixture.call("ai.session.send", send).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::QuotaExceeded);
    fixture
        .call("ai.session.close", json!({"conversation": conversation}))
        .await
        .unwrap();
}
