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

fn chat_options(tools: &[&str]) -> Value {
    json!({
        "role": "smart",
        "messages": [{"role": "user", "content": "hello"}],
        "tools": tools,
    })
}

#[tokio::test]
async fn roles_list_only_the_allowed_roles_and_a_substituted_role_is_not_published() {
    let fixture = Fixture::new(Some(&ai_manifest()), &[])
        .await
        .with_consent(Consent::allow_all());
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
    let fixture = Fixture::new(Some(&ai_manifest()), &[])
        .await
        .with_consent(substituted);
    let roles = fixture.call("ai.roles", Value::Null).await.unwrap();
    assert_eq!(roles["roles"].as_array().unwrap().len(), 1);
    assert_eq!(roles["roles"][0]["id"], json!("fast"));
}

#[tokio::test]
async fn a_denied_role_refuses_the_chat_with_permission_denied() {
    let mut denied = Consent::allow_all();
    denied.set(Right::scoped("ai.role", "smart"), Decision::Deny);
    let fixture = Fixture::new(Some(&ai_manifest()), &[])
        .await
        .with_consent(denied);
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
    let fixture = Fixture::new(Some(&ai_manifest()), &[])
        .await
        .with_consent(substituted);
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
    let fixture = Fixture::new(Some(&ai_manifest()), &[])
        .await
        .with_consent(Consent::allow_all());
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
    let fixture = Fixture::new(Some(&ai_manifest()), &[])
        .await
        .with_consent(Consent::allow_all());
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
            json!({"chat": chat, "callId": "call-0", "result": {"summary": "ok"}}),
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
}

#[tokio::test]
async fn a_replayed_or_foreign_tool_result_is_rejected() {
    let fixture = Fixture::new(Some(&ai_manifest()), &[])
        .await
        .with_consent(Consent::allow_all());
    fixture
        .call("ai.tools.handle", json!({"name": "summarizeSelection"}))
        .await
        .unwrap();
    let (chat, _stream, mut reader) =
        start_chat(&fixture, chat_options(&["summarizeSelection"])).await;
    frame(&mut reader).await;
    frame(&mut reader).await;
    let result = json!({"chat": chat, "callId": "call-0", "result": 1});
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
    let foreign = json!({"chat": chat + 9999, "callId": "call-0", "result": 1});
    assert_eq!(
        fixture
            .call("ai.tools.result", foreign)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Closed
    );
    let unknown = json!({"chat": chat, "callId": "call-9", "result": 1});
    assert_eq!(
        fixture
            .call("ai.tools.result", unknown)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Closed
    );
}

#[tokio::test]
async fn a_result_for_a_different_tool_name_is_rejected_without_burning_the_call() {
    let fixture = Fixture::new(Some(&ai_manifest()), &[])
        .await
        .with_consent(Consent::allow_all());
    fixture
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
}

#[tokio::test]
async fn results_for_a_tool_that_was_unregistered_are_rejected() {
    let fixture = Fixture::new(Some(&ai_manifest()), &[])
        .await
        .with_consent(Consent::allow_all());
    let registered = fixture
        .call("ai.tools.handle", json!({"name": "summarizeSelection"}))
        .await
        .unwrap();
    let (chat, _stream, mut reader) =
        start_chat(&fixture, chat_options(&["summarizeSelection"])).await;
    frame(&mut reader).await;
    frame(&mut reader).await;
    let registration = registered["registration"].as_u64().unwrap();
    fixture
        .call("ai.tools.unhandle", json!({"registration": registration}))
        .await
        .unwrap();
    let result = json!({"chat": chat, "callId": "call-0", "result": 1});
    // The teardown of the pending calls is only a moment away; wait for it, bounded.
    for _ in 0..250 {
        if fixture
            .call("ai.tools.result", result.clone())
            .await
            .is_err_and(|error| error.code == ErrorCode::Closed)
        {
            break;
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    assert_eq!(
        fixture
            .call("ai.tools.result", result)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Closed
    );
}

#[tokio::test]
async fn breaking_the_iterator_cancels_the_chat() {
    let fixture = Fixture::new(Some(&ai_manifest()), &[])
        .await
        .with_consent(Consent::allow_all());
    fixture
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
    // Sooner or later the producer tears the chat down; from then on every result is Closed.
    let result = json!({"chat": chat, "callId": "call-0", "result": 1});
    for _ in 0..250 {
        match fixture.call("ai.tools.result", result.clone()).await {
            // Not torn down yet; a burned call answers Closed on the very next try.
            Ok(_) => {}
            Err(error) if error.code == ErrorCode::Closed => break,
            Err(error) => panic!("unexpected error: {error}"),
        }
        tokio::time::sleep(Duration::from_millis(20)).await;
    }
    // After the teardown every later result is Closed, deterministically.
    assert_eq!(
        fixture
            .call("ai.tools.result", result)
            .await
            .unwrap_err()
            .code,
        ErrorCode::Closed
    );
}

#[tokio::test]
async fn a_chat_naming_a_tool_without_a_handler_is_an_invalid_argument() {
    let fixture = Fixture::new(Some(&ai_manifest()), &[])
        .await
        .with_consent(Consent::allow_all());
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
    let fixture = Fixture::new(Some(&ai_manifest()), &[])
        .await
        .with_consent(Consent::allow_all());
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
    let fixture = Fixture::new(Some(&ai_manifest()), &[])
        .await
        .with_consent(denied);
    assert_eq!(
        fixture
            .call("ai.tools.handle", json!({"name": "summarizeSelection"}))
            .await
            .unwrap_err()
            .code,
        ErrorCode::PermissionDenied
    );
    let substituted = consent(Decision::Substitute);
    let fixture = Fixture::new(Some(&ai_manifest()), &[])
        .await
        .with_consent(substituted);
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
}
