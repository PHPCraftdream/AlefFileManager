// SPDX-License-Identifier: MIT OR Apache-2.0
//! What the clipboard does with the decision of the user on reading it: the clipboard of the user,
//! a clipboard of the application's own, or an error.
use alef_core::{
    registry::command::Reply,
    security::consent::{Consent, Decision, Right},
    ErrorCode,
};
use alef_modules::ClipboardBackend;
use bytes::Bytes;
use serde_json::Value;

use crate::common::Fixture;

const MANIFEST: &str = include_str!("../fixtures/app.ktav");

fn reading() -> String {
    MANIFEST
        .replace('\r', "")
        .replace("read: false", "read: true")
}

async fn fixture(decision: Decision) -> Fixture {
    let mut consent = Consent::undecided();
    consent.set(Right::plain("clipboard.read"), decision);
    Fixture::new(Some(&reading()), &[])
        .await
        .with_consent(consent)
}

async fn write(fixture: &Fixture, command: &str, body: &[u8]) {
    fixture
        .call_reply(command, Value::Null, Some(Bytes::copy_from_slice(body)))
        .await
        .expect(command);
}

async fn read(fixture: &Fixture, command: &str) -> Result<Bytes, alef_core::AlefError> {
    match fixture.call_reply(command, Value::Null, None).await? {
        Reply::Bytes(bytes) => Ok(bytes),
        other => panic!("{command}: {other:?}"),
    }
}

#[tokio::test]
async fn an_allowed_clipboard_is_the_clipboard_of_the_user() {
    let fixture = fixture(Decision::Allow).await;
    write(&fixture, "clipboard.writeText", b"for real").await;
    assert_eq!(fixture.clipboard.read_text().unwrap(), "for real");
    assert_eq!(
        read(&fixture, "clipboard.readText").await.unwrap(),
        "for real".as_bytes()
    );
}

#[tokio::test]
async fn a_stand_in_is_a_clipboard_of_the_applications_own_and_the_users_is_never_touched() {
    let fixture = fixture(Decision::Substitute).await;
    fixture
        .clipboard
        .write_text("what the user copied")
        .unwrap();
    assert!(
        read(&fixture, "clipboard.readText")
            .await
            .unwrap()
            .is_empty(),
        "the application does not see what the user copied"
    );
    write(&fixture, "clipboard.writeText", b"from the application").await;
    assert_eq!(
        read(&fixture, "clipboard.readText").await.unwrap(),
        "from the application".as_bytes(),
        "it finds its own writes where it left them"
    );
    write(&fixture, "clipboard.writeHtml", b"<b>x</b>").await;
    assert_eq!(
        read(&fixture, "clipboard.readHtml").await.unwrap(),
        "<b>x</b>".as_bytes()
    );
    assert!(
        read(&fixture, "clipboard.readText")
            .await
            .unwrap()
            .is_empty(),
        "one thing at a time, like the real one"
    );
    assert_eq!(
        fixture.clipboard.read_text().unwrap(),
        "what the user copied",
        "and the clipboard of the user is as it was"
    );
}

#[tokio::test]
async fn reading_denied_is_an_error_and_writing_still_goes_to_the_users_clipboard() {
    let fixture = fixture(Decision::Deny).await;
    for command in [
        "clipboard.readText",
        "clipboard.readHtml",
        "clipboard.readImage",
    ] {
        assert_eq!(
            read(&fixture, command).await.unwrap_err().code,
            ErrorCode::PermissionDenied,
            "{command}"
        );
    }
    write(&fixture, "clipboard.writeText", b"copied anyway").await;
    assert_eq!(
        fixture.clipboard.read_text().unwrap(),
        "copied anyway",
        "writing was never the right in question"
    );
}

#[tokio::test]
async fn an_application_that_asks_for_no_reading_still_writes_for_real() {
    let fixture = Fixture::new(None, &[]).await;
    write(&fixture, "clipboard.writeText", b"plain").await;
    assert_eq!(fixture.clipboard.read_text().unwrap(), "plain");
}
