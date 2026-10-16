// SPDX-License-Identifier: MIT OR Apache-2.0
//! The commands of `ai` through the registry, against the deterministic provider: the surface of
//! docs/stages/m9-ai.md §2 with the limits of §6.
use std::time::Duration;

use alef_core::{
    ids::StreamId,
    protocol::frame::Frame,
    security::consent::{Consent, Decision, Right},
    session::streams::StreamReader,
    ErrorCode,
};
use alef_modules::ai;
use serde_json::{json, Value};

use super::common::Fixture;

const BASE_MANIFEST: &str = include_str!("../fixtures/app.ktav");

/// The fixture manifest with an `ai` section: two roles and one tool (docs/stages/m9-ai.md §3).
fn ai_manifest() -> String {
    BASE_MANIFEST.replace(
        "    app: {",
        r#"    ai: {
        roles: [smart, fast]
        tools: [
            {
                name: summarizeSelection
                description: Summarize selected text
                schema: {
                    type: object
                    properties: {
                        text: {
                            type: string
                        }
                    }
                    required: [text]
                    additionalProperties: false
                }
            }
        ]
    }
    app: {"#,
    )
}

/// The roles allowed, the tool decided separately.
fn consent(tool: Decision) -> Consent {
    let mut consent = Consent::allow_all();
    consent.set(Right::scoped("ai.tool", "summarizeSelection"), tool);
    consent
}

/// A fixture whose application key is its own: the per-application budgets of the `ai` module
/// never couple two tests that run side by side.
async fn ai_fixture(app: &'static str, consent: Consent) -> Fixture {
    ai_fixture_with_manifest(app, consent, false).await
}

/// Fixture that can explicitly declare execution builtins for consent-path tests.
async fn ai_fixture_with_manifest(
    app: &'static str,
    consent: Consent,
    declare_exec: bool,
) -> Fixture {
    let manifest = if declare_exec {
        BASE_MANIFEST.replace(
            "    app: {",
            "    ai: {\n        roles: [smart, fast]\n        tools: []\n        builtin: [\n            {\n                name: exec\n            }\n        ]\n    }\n    app: {",
        )
    } else {
        ai_manifest()
    };
    let root = std::env::current_exe()
        .unwrap()
        .parent()
        .unwrap()
        .join("alef-modules-paths");
    Fixture::new_sharing_authority(
        &root,
        Some(&manifest),
        std::sync::Arc::new(alef_modules::MemorySecrets::default()),
        alef_core::security::authority::Authority::SignedPackage {
            fingerprint: app.to_owned(),
        },
    )
    .await
    .with_consent(consent)
}

/// One frame of the chat stream, within a bounded wait: a test fails, it does not hang.
async fn frame(reader: &mut StreamReader) -> Frame {
    tokio::time::timeout(Duration::from_secs(5), reader.next_frame())
        .await
        .expect("a frame in time")
        .expect("the stream is open")
}

/// The payload of the first event of `name` the host was given, within a bounded wait.
async fn event(fixture: &Fixture, name: &str) -> Value {
    for _ in 0..250 {
        let found = fixture
            .host
            .events
            .lock()
            .unwrap()
            .iter()
            .find(|(_, sent, _)| sent == name)
            .map(|(_, _, payload)| payload.clone());
        if let Some(payload) = found {
            return payload;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    panic!("the event {name} never came");
}

/// Starts a chat and returns its chat id, its stream id and the reader of the stream.
async fn start_chat(fixture: &Fixture, options: Value) -> (u64, u64, StreamReader) {
    let reply = fixture
        .call("ai.chat", options)
        .await
        .expect("the chat starts");
    let chat = reply["chat"].as_u64().expect("a chat id");
    let stream = reply["stream"].as_u64().expect("a stream id");
    let reader = fixture
        .session()
        .streams()
        .reader(StreamId(stream))
        .expect("an outgoing stream");
    (chat, stream, reader)
}

/// Starts a command-backed stream and returns its id and reader.
async fn start_mode(fixture: &Fixture, command: &str, options: Value) -> (u64, StreamReader) {
    let reply = fixture
        .call(command, options)
        .await
        .expect("the mode starts");
    let stream = reply["stream"].as_u64().expect("a stream id");
    let reader = fixture
        .session()
        .streams()
        .reader(StreamId(stream))
        .expect("an outgoing stream");
    (stream, reader)
}

/// Drains a stream to its terminal frame, collecting text; provider errors fail the test.
async fn drain_text(reader: &mut StreamReader) -> String {
    let mut text = String::new();
    for _ in 0..32 {
        match frame(reader).await {
            Frame::Json(value) if value["type"] == "text" => {
                text.push_str(value["text"].as_str().expect("text payload"));
            }
            Frame::Json(value) if value["type"] == "error" => panic!("provider error: {value}"),
            Frame::Binary(bytes) => panic!("unexpected binary frame: {} bytes", bytes.len()),
            Frame::Error(error) => panic!("transport error: {error}"),
            Frame::End => return text,
            other => {}
        }
    }
    panic!("stream exceeded the frame bound");
}

fn chat_options(tools: &[&str]) -> Value {
    json!({
        "role": "smart",
        "messages": [{"role": "user", "content": "hello"}],
        "tools": tools,
    })
}

#[tokio::test]
async fn roles_list_only_the_allowed_roles_and_a_substituted_role_is_not_published() {
    let fixture = ai_fixture("roles", Consent::allow_all()).await;
    let roles = fixture.call("ai.roles", Value::Null).await.unwrap();
    assert_eq!(
        roles,
        json!({"roles": [
            {"id": "smart", "title": "Smart",
             "description": "the careful, slower role",
             "capabilities": ["text", "tool_calls", "usage", "cancel"]},
            {"id": "fast", "title": "Fast",
             "description": "the quick, lighter role",
             "capabilities": ["text", "usage"]},
        ]})
    );
    let mut substituted = Consent::undecided();
    substituted.set(Right::scoped("ai.role", "smart"), Decision::Substitute);
    substituted.set(Right::scoped("ai.role", "fast"), Decision::Allow);
    let fixture = ai_fixture("substituted-role", substituted).await;
    let roles = fixture.call("ai.roles", Value::Null).await.unwrap();
    assert_eq!(roles["roles"].as_array().unwrap().len(), 1);
    assert_eq!(roles["roles"][0]["id"], json!("fast"));
}

#[tokio::test]
async fn a_denied_role_refuses_the_chat_with_permission_denied() {
    let mut denied = Consent::allow_all();
    denied.set(Right::scoped("ai.role", "smart"), Decision::Deny);
    let fixture = ai_fixture("denied-role", denied).await;
    let error = fixture
        .call("ai.chat", chat_options(&[]))
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::PermissionDenied);
}

#[tokio::test]
async fn a_substituted_role_answers_not_available_without_reaching_the_provider() {
    let mut substituted = Consent::allow_all();
    substituted.set(Right::scoped("ai.role", "smart"), Decision::Substitute);
    let fixture = ai_fixture("substituted-chat-role", substituted).await;
    let before = fixture.ai.chats_started();
    let error = fixture
        .call("ai.chat", chat_options(&[]))
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::NotAvailable);
    assert_eq!(error.message, "the role is not available");
    assert_eq!(fixture.ai.chats_started(), before);
}

#[tokio::test]
async fn a_chat_streams_text_usage_and_exactly_one_completed_end() {
    let fixture = ai_fixture("streams", Consent::allow_all()).await;
    let (_chat, _stream, mut reader) = start_chat(&fixture, chat_options(&[])).await;
    assert_eq!(
        frame(&mut reader).await,
        Frame::Json(json!({"type": "text", "text": "Thinking"}))
    );
    assert_eq!(
        frame(&mut reader).await,
        Frame::Json(json!({
            "type": "usage",
            "inputTokens": 10,
            "outputTokens": 5,
            "cacheReadTokens": null,
            "cacheCreationTokens": null,
            "estimated": true,
            "complete": true,
        }))
    );
    assert_eq!(
        frame(&mut reader).await,
        Frame::Json(json!({"type": "end", "reason": "completed"}))
    );
    assert_eq!(frame(&mut reader).await, Frame::End);
    let after = tokio::time::timeout(Duration::from_millis(300), reader.next_frame()).await;
    assert_eq!(
        after.expect("the wait ends"),
        None,
        "nothing follows the end"
    );
}

#[tokio::test]
async fn a_tool_call_round_trip_reaches_the_handler_and_uses_its_result() {
    let fixture = ai_fixture("round-trip", Consent::allow_all()).await;
    let registered = fixture
        .call("ai.tools.handle", json!({"name": "summarizeSelection"}))
        .await
        .unwrap();
    assert_eq!(registered["inert"], json!(false));
    let (chat, _stream, mut reader) =
        start_chat(&fixture, chat_options(&["summarizeSelection"])).await;
    assert_eq!(
        frame(&mut reader).await,
        Frame::Json(json!({"type": "text", "text": "Thinking"}))
    );
    assert_eq!(
        frame(&mut reader).await,
        Frame::Json(json!({
            "type": "tool_call",
            "id": "call-0",
            "name": "summarizeSelection",
            "arguments": {"input": "ping"},
        }))
    );
    let payload = event(&fixture, "ai.tool_call").await;
    assert_eq!(
        payload,
        json!({
            "chat": chat,
            "callId": "call-0",
            "name": "summarizeSelection",
            "arguments": {"input": "ping"},
        })
    );
    let answered = fixture
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
    assert_eq!(answered, Value::Null);
    assert_eq!(
        frame(&mut reader).await,
        Frame::Json(json!({"type": "text", "text": "Saw 1 tool result(s): ok"}))
    );
    assert_eq!(
        frame(&mut reader).await,
        Frame::Json(json!({
            "type": "usage",
            "inputTokens": 15,
            "outputTokens": 8,
            "cacheReadTokens": null,
            "cacheCreationTokens": null,
            "estimated": true,
            "complete": true,
        }))
    );
    assert_eq!(
        frame(&mut reader).await,
        Frame::Json(json!({"type": "end", "reason": "completed"}))
    );
    assert_eq!(frame(&mut reader).await, Frame::End);
    // The registration of this test must not leak into the next one.
    let registration = registered["registration"].as_u64().unwrap();
    let _ = fixture
        .call("ai.tools.unhandle", json!({"registration": registration}))
        .await;
}

#[tokio::test]
async fn a_replayed_or_foreign_tool_result_is_rejected() {
    let fixture = ai_fixture("replay", Consent::allow_all()).await;
    let registered = fixture
        .call("ai.tools.handle", json!({"name": "summarizeSelection"}))
        .await
        .unwrap();
    let (chat, _stream, mut reader) =
        start_chat(&fixture, chat_options(&["summarizeSelection"])).await;
    frame(&mut reader).await;
    frame(&mut reader).await;
    let result = json!({
        "chat": chat,
        "callId": "call-0",
        "name": "summarizeSelection",
        "result": 1,
    });
    fixture
        .call("ai.tools.result", result.clone())
        .await
        .unwrap();
    // The same answer never passes twice.
    assert_eq!(
        fixture
            .call("ai.tools.result", result)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Closed
    );
    // A chat that is not ours looks exactly like one that is over.
    let foreign = json!({
        "chat": chat + 9999,
        "callId": "call-0",
        "name": "summarizeSelection",
        "result": 1,
    });
    assert_eq!(
        fixture
            .call("ai.tools.result", foreign)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Closed
    );
    let unknown = json!({
        "chat": chat,
        "callId": "call-9",
        "name": "summarizeSelection",
        "result": 1,
    });
    assert_eq!(
        fixture
            .call("ai.tools.result", unknown)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Closed
    );
    // The registration of this test must not leak into the next one.
    let registration = registered["registration"].as_u64().unwrap();
    let _ = fixture
        .call("ai.tools.unhandle", json!({"registration": registration}))
        .await;
}

#[tokio::test]
async fn a_result_for_a_different_tool_name_is_rejected_without_burning_the_call() {
    let fixture = ai_fixture("wrong-name", Consent::allow_all()).await;
    let registered = fixture
        .call("ai.tools.handle", json!({"name": "summarizeSelection"}))
        .await
        .unwrap();
    let (chat, _stream, mut reader) =
        start_chat(&fixture, chat_options(&["summarizeSelection"])).await;
    frame(&mut reader).await;
    frame(&mut reader).await;
    // A result claiming another tool never burns the call.
    let wrong = json!({
        "chat": chat,
        "callId": "call-0",
        "name": "otherTool",
        "result": 1,
    });
    assert_eq!(
        fixture
            .call("ai.tools.result", wrong)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Closed
    );
    let right = json!({
        "chat": chat,
        "callId": "call-0",
        "name": "summarizeSelection",
        "result": {"summary": "ok"},
    });
    fixture.call("ai.tools.result", right).await.unwrap();
    assert_eq!(
        frame(&mut reader).await,
        Frame::Json(json!({"type": "text", "text": "Saw 1 tool result(s): ok"}))
    );
    // The registration of this test must not leak into the next one.
    let registration = registered["registration"].as_u64().unwrap();
    let _ = fixture
        .call("ai.tools.unhandle", json!({"registration": registration}))
        .await;
}

#[tokio::test]
async fn results_for_a_tool_that_was_unregistered_are_rejected() {
    let fixture = ai_fixture("unregister", Consent::allow_all()).await;
    let registered = fixture
        .call("ai.tools.handle", json!({"name": "summarizeSelection"}))
        .await
        .unwrap();
    let (chat, _stream, mut reader) =
        start_chat(&fixture, chat_options(&["summarizeSelection"])).await;
    frame(&mut reader).await;
    frame(&mut reader).await;
    let registration = registered["registration"].as_u64().unwrap();
    // The probe sees the live registration before anything is removed.
    assert!(
        ai::tool_is_registered(&fixture.session(), registration),
        "the probe sees the live registration"
    );
    fixture
        .call("ai.tools.unhandle", json!({"registration": registration}))
        .await
        .unwrap();
    // The resource is gone deterministically; no retry loop is needed.
    let mut gone = false;
    for _ in 0..250 {
        if !ai::tool_is_registered(&fixture.session(), registration) {
            gone = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(gone, "the registration was never removed");
    // Exactly one answer, deterministically: the call was torn down with the tool.
    let result = json!({
        "chat": chat,
        "callId": "call-0",
        "name": "summarizeSelection",
        "result": 1,
    });
    assert_eq!(
        fixture
            .call("ai.tools.result", result.clone())
            .await
            .unwrap_err()
            .code,
        ErrorCode::Closed
    );
    // Re-registering the same name issues a NEW registration generation; it must not satisfy the
    // old pending call, and its result must not reach the provider either.
    let registered_again = fixture
        .call("ai.tools.handle", json!({"name": "summarizeSelection"}))
        .await
        .unwrap();
    assert_eq!(registered_again["inert"], json!(false));
    let registration_again = registered_again["registration"].as_u64().unwrap();
    assert_eq!(
        fixture
            .call("ai.tools.result", result)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Closed
    );
    let after = tokio::time::timeout(Duration::from_millis(300), reader.next_frame()).await;
    assert!(
        after.is_err(),
        "the provider never receives the stale result"
    );
    // Clean up both registrations.
    let _ = fixture
        .call("ai.tools.unhandle", json!({"registration": registration}))
        .await;
    let _ = fixture
        .call(
            "ai.tools.unhandle",
            json!({"registration": registration_again}),
        )
        .await;
}

#[tokio::test]
async fn breaking_the_iterator_cancels_the_chat() {
    let fixture = ai_fixture("break", Consent::allow_all()).await;
    let registered = fixture
        .call("ai.tools.handle", json!({"name": "summarizeSelection"}))
        .await
        .unwrap();
    let (chat, stream, mut reader) =
        start_chat(&fixture, chat_options(&["summarizeSelection"])).await;
    frame(&mut reader).await;
    frame(&mut reader).await;
    fixture
        .session()
        .streams()
        .close(StreamId(stream))
        .expect("the stream closes");
    // The producer tears the chat down deterministically; the probe sees it without burning the call.
    let mut gone = false;
    for _ in 0..250 {
        if !ai::chat_is_pending(&fixture.session(), chat) {
            gone = true;
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert!(gone, "the chat was never torn down");
    // After the teardown the result is Closed, deterministically, exactly once.
    let result = json!({
        "chat": chat,
        "callId": "call-0",
        "name": "summarizeSelection",
        "result": 1,
    });
    assert_eq!(
        fixture
            .call("ai.tools.result", result)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Closed
    );
    // The registration of this test must not leak into the next one.
    let registration = registered["registration"].as_u64().unwrap();
    let _ = fixture
        .call("ai.tools.unhandle", json!({"registration": registration}))
        .await;
}

#[tokio::test]
async fn a_chat_naming_a_tool_without_a_handler_is_an_invalid_argument() {
    let fixture = ai_fixture("missing-handler", Consent::allow_all()).await;
    let before = fixture.session().resources().len();
    let error = fixture
        .call("ai.chat", chat_options(&["summarizeSelection"]))
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidArgument);
    assert!(error.message.contains("summarizeSelection"));
    assert_eq!(fixture.session().resources().len(), before);
}

#[tokio::test]
async fn a_duplicate_tool_registration_is_busy_and_unregister_is_idempotent() {
    let fixture = ai_fixture("duplicate", Consent::allow_all()).await;
    let registered = fixture
        .call("ai.tools.handle", json!({"name": "summarizeSelection"}))
        .await
        .unwrap();
    assert_eq!(
        fixture
            .call("ai.tools.handle", json!({"name": "summarizeSelection"}))
            .await
            .unwrap_err()
            .code,
        ErrorCode::Busy
    );
    let registration = registered["registration"].as_u64().unwrap();
    let first = fixture
        .call("ai.tools.unhandle", json!({"registration": registration}))
        .await
        .unwrap();
    let second = fixture
        .call("ai.tools.unhandle", json!({"registration": registration}))
        .await
        .unwrap();
    assert_eq!((first, second), (Value::Null, Value::Null));
    // After the unregister the chat no longer sees a handler either.
    assert_eq!(
        fixture
            .call("ai.chat", chat_options(&["summarizeSelection"]))
            .await
            .unwrap_err()
            .code,
        ErrorCode::InvalidArgument
    );
}

#[tokio::test]
async fn a_denied_tool_registration_is_permission_denied_and_an_inert_one_round_trips() {
    let denied = consent(Decision::Deny);
    let fixture = ai_fixture("tool-denied", denied).await;
    assert_eq!(
        fixture
            .call("ai.tools.handle", json!({"name": "summarizeSelection"}))
            .await
            .unwrap_err()
            .code,
        ErrorCode::PermissionDenied
    );
    let substituted = consent(Decision::Substitute);
    let fixture = ai_fixture("tool-inert", substituted).await;
    let registered = fixture
        .call("ai.tools.handle", json!({"name": "summarizeSelection"}))
        .await
        .unwrap();
    assert_eq!(registered["inert"], json!(true));
    let (_chat, _stream, mut reader) =
        start_chat(&fixture, chat_options(&["summarizeSelection"])).await;
    assert_eq!(
        frame(&mut reader).await,
        Frame::Json(json!({"type": "text", "text": "Thinking"}))
    );
    assert_eq!(
        frame(&mut reader).await,
        Frame::Json(json!({
            "type": "usage",
            "inputTokens": 10,
            "outputTokens": 5,
            "cacheReadTokens": null,
            "cacheCreationTokens": null,
            "estimated": true,
            "complete": true,
        }))
    );
    assert_eq!(
        frame(&mut reader).await,
        Frame::Json(json!({"type": "end", "reason": "completed"}))
    );
    assert_eq!(frame(&mut reader).await, Frame::End);
    // The inert tool was never offered: no tool call reached the handler.
    assert!(fixture
        .host
        .events
        .lock()
        .unwrap()
        .iter()
        .all(|(_, name, _)| name != "ai.tool_call"));
    // The registration of this test must not leak into the next one.
    let registration = registered["registration"].as_u64().unwrap();
    let _ = fixture
        .call("ai.tools.unhandle", json!({"registration": registration}))
        .await;
}

#[tokio::test]
async fn a_result_of_null_is_a_valid_value() {
    let fixture = ai_fixture("null-result", Consent::allow_all()).await;
    let registered = fixture
        .call("ai.tools.handle", json!({"name": "summarizeSelection"}))
        .await
        .unwrap();
    let (chat, _stream, mut reader) =
        start_chat(&fixture, chat_options(&["summarizeSelection"])).await;
    assert_eq!(
        frame(&mut reader).await,
        Frame::Json(json!({"type": "text", "text": "Thinking"}))
    );
    assert_eq!(
        frame(&mut reader).await,
        Frame::Json(json!({
            "type": "tool_call",
            "id": "call-0",
            "name": "summarizeSelection",
            "arguments": {"input": "ping"},
        }))
    );
    // An explicit null is a value the handler returned, not a missing one.
    let answered = fixture
        .call(
            "ai.tools.result",
            json!({
                "chat": chat,
                "callId": "call-0",
                "name": "summarizeSelection",
                "result": null,
            }),
        )
        .await
        .unwrap();
    assert_eq!(answered, Value::Null);
    assert_eq!(
        frame(&mut reader).await,
        Frame::Json(json!({"type": "text", "text": "Saw 1 tool result(s): null"}))
    );
    assert_eq!(
        frame(&mut reader).await,
        Frame::Json(json!({
            "type": "usage",
            "inputTokens": 15,
            "outputTokens": 8,
            "cacheReadTokens": null,
            "cacheCreationTokens": null,
            "estimated": true,
            "complete": true,
        }))
    );
    assert_eq!(
        frame(&mut reader).await,
        Frame::Json(json!({"type": "end", "reason": "completed"}))
    );
    assert_eq!(frame(&mut reader).await, Frame::End);
    // The registration of this test must not leak into the next one.
    let registration = registered["registration"].as_u64().unwrap();
    let _ = fixture
        .call("ai.tools.unhandle", json!({"registration": registration}))
        .await;
}

#[tokio::test]
async fn session_history_is_owned_by_runtime_and_close_forgets_the_session() {
    let fixture = ai_fixture("session-history", Consent::allow_all()).await;
    let opened = fixture
        .call("ai.session.open", json!({"role":"smart"}))
        .await
        .unwrap();
    let conversation = opened["conversation"].as_u64().unwrap();
    let send = |content: &str| json!({"conversation":conversation,"messages":[{"role":"user","content":content}],"tools":[]});
    let (_, mut first) = start_mode(&fixture, "ai.session.send", send("alpha")).await;
    let first_text = drain_text(&mut first).await;
    let (_, mut second) = start_mode(&fixture, "ai.session.send", send("beta")).await;
    let history_text = drain_text(&mut second).await;
    let history_prefix = history_text
        .strip_prefix("History ")
        .expect("serialized history");
    let history: Value = serde_json::from_str(history_prefix).expect("history JSON");
    let messages = history.as_array().expect("history array");
    assert_eq!(messages[0]["role"], "user");
    assert_eq!(messages[0]["content"], "alpha");
    assert_eq!(messages[1]["role"], "assistant");
    assert_eq!(messages[1]["content"], first_text);
    assert_eq!(messages[2]["role"], "user");
    assert_eq!(messages[2]["content"], "beta");
    fixture
        .call("ai.session.close", json!({"conversation":conversation}))
        .await
        .unwrap();
    assert_eq!(
        fixture
            .call("ai.session.send", send("after close"))
            .await
            .unwrap_err()
            .code,
        ErrorCode::Closed
    );
}

#[tokio::test]
async fn one_session_rejects_a_second_active_send() {
    let fixture = ai_fixture("session-busy", Consent::allow_all()).await;
    let opened = fixture
        .call("ai.session.open", json!({"role":"smart"}))
        .await
        .unwrap();
    let conversation = opened["conversation"].as_u64().unwrap();
    let options = json!({"conversation":conversation,"messages":[{"role":"user","content":"alpha"}],"tools":[]});
    let gate = fixture.ai.hold_next_chat();
    let (_, mut reader) = start_mode(&fixture, "ai.session.send", options.clone()).await;
    assert_eq!(
        fixture
            .call("ai.session.send", options)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Busy
    );
    gate.release();
    drain_text(&mut reader).await;
    fixture
        .call("ai.session.close", json!({"conversation":conversation}))
        .await
        .unwrap();
}

#[tokio::test]
async fn a_session_without_tools_does_not_dispatch_registered_app_tools() {
    let fixture = ai_fixture("session-no-tools", Consent::allow_all()).await;
    let registered = fixture
        .call("ai.tools.handle", json!({"name":"summarizeSelection"}))
        .await
        .unwrap();
    let opened = fixture
        .call("ai.session.open", json!({"role":"smart"}))
        .await
        .unwrap();
    let conversation = opened["conversation"].as_u64().unwrap();
    let (_, mut reader) = start_mode(&fixture, "ai.session.send", json!({"conversation":conversation,"messages":[{"role":"user","content":"alpha"}],"tools":[]})).await;
    let mut tool_call = false;
    for _ in 0..32 {
        match frame(&mut reader).await {
            Frame::Json(value) => {
                assert_ne!(value["type"], json!("tool_call"));
                tool_call |= value["type"] == "tool_call";
            }
            Frame::End => break,
            Frame::Binary(bytes) => panic!(
                "unexpected binary frame in no-tools test: {} bytes",
                bytes.len()
            ),
            Frame::Error(error) => panic!("unexpected stream error in no-tools test: {error}"),
        }
    }
    assert!(!tool_call);
    assert!(fixture
        .host
        .events
        .lock()
        .unwrap()
        .iter()
        .all(|(_, name, _)| name != "ai.tool_call"));
    fixture
        .call("ai.session.close", json!({"conversation":conversation}))
        .await
        .unwrap();
    fixture
        .call(
            "ai.tools.unhandle",
            json!({"registration":registered["registration"]}),
        )
        .await
        .unwrap();
}

#[tokio::test]
async fn builtin_fs_read_is_scoped_and_never_emits_an_app_tool_call() {
    let scratch = tempfile::tempdir().unwrap();
    let allowed = scratch.path().join("allowed");
    std::fs::create_dir(&allowed).unwrap();
    let sentinel = allowed.join("sentinel.txt");
    std::fs::write(&sentinel, "builtin sentinel").unwrap();
    let outside = scratch.path().join("outside.txt");
    std::fs::write(&outside, "outside").unwrap();
    // Ktav values are unquoted: quotes would become part of the pattern.
    let scope = format!("{}/*", allowed.display());
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
            fingerprint: "builtin-fs-read".to_owned(),
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
    assert_eq!(
        frame(&mut reader).await,
        Frame::Json(json!({"type":"text","text":"Thinking"}))
    );
    let response = drain_text(&mut reader).await;
    assert!(response.starts_with("Saw 1 tool result(s):"));
    assert!(response.contains("builtin sentinel"));
    assert!(fixture
        .host
        .events
        .lock()
        .unwrap()
        .iter()
        .all(|(_, name, _)| name != "ai.tool_call"));

    let options = json!({
        "role": "smart",
        "messages": [{"role":"user", "content": format!("{{\"path\":{}}}", serde_json::to_string(&outside.to_string_lossy()).unwrap())}],
        "builtin": ["fs.read"],
    });
    let (_, mut reader) = start_mode(&fixture, "ai.request", options).await;
    assert_eq!(
        frame(&mut reader).await,
        Frame::Json(json!({"type":"text","text":"Thinking"}))
    );
    assert!(
        matches!(frame(&mut reader).await, Frame::Error(error) if error.code == ErrorCode::PermissionDenied)
    );
    assert!(fixture
        .host
        .events
        .lock()
        .unwrap()
        .iter()
        .all(|(_, name, _)| name != "ai.tool_call"));
}

#[tokio::test]
async fn denied_or_undeclared_execution_builtin_is_rejected_before_provider_start() {
    let mut denied = Consent::allow_all();
    denied.set(Right::plain("ai.builtin.exec"), Decision::Deny);
    let fixture = ai_fixture_with_manifest("builtin-exec-denied", denied, true).await;
    let before = fixture.ai.chats_started();
    let error = fixture
        .call(
            "ai.request",
            json!({
                "role": "smart",
                "messages": [{"role":"user", "content":"hello"}],
                "builtin": ["exec"],
            }),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::PermissionDenied);
    assert_eq!(fixture.ai.chats_started(), before);

    let fixture =
        ai_fixture_with_manifest("builtin-exec-undeclared", Consent::allow_all(), false).await;
    let before = fixture.ai.chats_started();
    let error = fixture
        .call(
            "ai.request",
            json!({
                "role": "smart",
                "messages": [{"role":"user", "content":"hello"}],
                "builtin": ["exec"],
            }),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::PermissionDenied);
    assert_eq!(fixture.ai.chats_started(), before);
}

#[tokio::test]
async fn stateless_request_starts_and_drains_a_response() {
    let fixture = ai_fixture("stateless-request", Consent::allow_all()).await;
    let (_, mut reader) = start_mode(&fixture, "ai.request", chat_options(&[])).await;
    assert_eq!(drain_text(&mut reader).await, "Thinking");
}
