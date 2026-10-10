// SPDX-License-Identifier: MIT OR Apache-2.0
//! What the modules of `app` and `shell` do with the decision of the user: give the real thing, a
//! stand-in the application cannot tell from it, or an error.
use std::{fs, path::Path};

use alef_core::{
    security::consent::{Consent, Decision, Right},
    ErrorCode,
};
use serde_json::{json, Value};

use crate::common::Fixture;

const MANIFEST: &str = include_str!("../fixtures/app.ktav");

fn consent(decisions: &[(Right, Decision)]) -> Consent {
    let mut consent = Consent::undecided();
    for (right, decision) in decisions {
        consent.set(right.clone(), *decision);
    }
    consent
}

/// The environment variables `ALEF_CONSENT_ALLOWED`, `ALEF_CONSENT_SUBSTITUTED` and
/// `ALEF_CONSENT_DENIED` are listed in the manifest and set in this process;
/// `ALEF_MODULES_TEST_UNSET` is listed and not set.
fn manifest() -> String {
    MANIFEST.replace('\r', "").replace(
        "env: [ PATH, ALEF_MODULES_TEST_UNSET ]",
        "env: [ ALEF_CONSENT_ALLOWED, ALEF_CONSENT_SUBSTITUTED, ALEF_CONSENT_DENIED, ALEF_MODULES_TEST_UNSET ]",
    )
}

fn environment() {
    // Set for every test of this binary; the values never change.
    for name in [
        "ALEF_CONSENT_ALLOWED",
        "ALEF_CONSENT_SUBSTITUTED",
        "ALEF_CONSENT_DENIED",
    ] {
        if std::env::var_os(name).is_none() {
            std::env::set_var(name, format!("value-of-{name}"));
        }
    }
}

async fn fixture(decisions: &[(Right, Decision)]) -> Fixture {
    environment();
    Fixture::new(Some(&manifest()), &[])
        .await
        .with_consent(consent(decisions))
}

fn env_right(name: &str) -> Right {
    Right::scoped("app.env", name)
}

fn all_four() -> [(Right, Decision); 4] {
    [
        (env_right("ALEF_CONSENT_ALLOWED"), Decision::Allow),
        (env_right("ALEF_CONSENT_SUBSTITUTED"), Decision::Substitute),
        (env_right("ALEF_CONSENT_DENIED"), Decision::Deny),
        (env_right("ALEF_MODULES_TEST_UNSET"), Decision::Allow),
    ]
}

#[tokio::test]
async fn an_environment_variable_is_given_as_a_stand_in_denied_or_for_real() {
    let fixture = fixture(&all_four()).await;
    let read = |name: &'static str| {
        let fixture = &fixture;
        async move { fixture.call("app.env", json!({ "name": name })).await }
    };
    assert_eq!(
        read("ALEF_CONSENT_ALLOWED").await.unwrap(),
        json!("value-of-ALEF_CONSENT_ALLOWED")
    );
    let unset = read("ALEF_MODULES_TEST_UNSET").await.unwrap();
    assert_eq!(unset, Value::Null);
    assert_eq!(
        read("ALEF_CONSENT_SUBSTITUTED").await.unwrap(),
        unset,
        "a stand-in answers exactly like a variable that is not set: no error, no other shape"
    );
    let denied = read("ALEF_CONSENT_DENIED").await.unwrap_err();
    assert_eq!(denied.code, ErrorCode::PermissionDenied);
    assert_eq!(
        denied,
        read("NOT_LISTED_AT_ALL").await.unwrap_err(),
        "a denial of the user is the error of a right the manifest does not list"
    );
}

#[tokio::test]
async fn the_whole_list_has_only_what_is_really_given() {
    let fixture = fixture(&all_four()).await;
    let all = fixture.call("app.envAll", Value::Null).await.unwrap();
    assert_eq!(
        all,
        json!({ "ALEF_CONSENT_ALLOWED": "value-of-ALEF_CONSENT_ALLOWED" })
    );
}

fn scope_of(directory: &Path) -> String {
    format!("{}/**", directory.display().to_string().replace('\\', "/"))
}

async fn shell_fixture(directory: &Path, decision: Decision) -> Fixture {
    let scope = scope_of(directory);
    let text = MANIFEST
        .replace('\r', "")
        .replace(
            "        openExternal: []",
            "        openExternal: [ https://example.com/docs/* ]",
        )
        .replace(
            "        read: []\n        write: []",
            &format!("        read: [ {scope} ]\n        write: [ {scope} ]"),
        );
    Fixture::new(Some(&text), &[]).await.with_consent(consent(&[
        (
            Right::scoped("shell.openExternal", "https://example.com/docs/*"),
            decision,
        ),
        (Right::scoped("fs.read", &scope), decision),
        (Right::scoped("fs.write", &scope), decision),
    ]))
}

#[tokio::test]
async fn a_stand_in_for_the_shell_does_nothing_and_answers_like_the_real_thing() {
    let directory = tempfile::tempdir().unwrap();
    let file = directory.path().join("note.txt");
    fs::write(&file, "x").unwrap();
    let url = "https://example.com/docs/guide";

    let real = shell_fixture(directory.path(), Decision::Allow).await;
    real.call("shell.openExternal", json!({ "url": url }))
        .await
        .unwrap();
    real.call("shell.openPath", json!({ "path": file.to_string_lossy() }))
        .await
        .unwrap();
    real.call("shell.trash", json!({ "path": file.to_string_lossy() }))
        .await
        .unwrap();
    assert_eq!(
        real.shell.asked().len(),
        3,
        "allowed: the desktop was asked"
    );

    let stand_in = shell_fixture(directory.path(), Decision::Substitute).await;
    assert_eq!(
        stand_in
            .call("shell.openExternal", json!({ "url": url }))
            .await
            .unwrap(),
        Value::Null
    );
    stand_in
        .call("shell.openPath", json!({ "path": file.to_string_lossy() }))
        .await
        .unwrap();
    stand_in
        .call(
            "shell.showInFolder",
            json!({ "path": file.to_string_lossy() }),
        )
        .await
        .unwrap();
    stand_in
        .call("shell.trash", json!({ "path": file.to_string_lossy() }))
        .await
        .unwrap();
    assert!(
        stand_in.shell.asked().is_empty(),
        "substituted: the desktop of the user was not asked"
    );
    assert!(file.exists(), "and nothing was thrown away");
    let missing = directory.path().join("never-was.txt");
    let error = stand_in
        .call("shell.trash", json!({ "path": missing.to_string_lossy() }))
        .await
        .unwrap_err();
    assert_eq!(
        error.code,
        ErrorCode::NotFound,
        "a stand-in has the same errors for the same mistakes"
    );

    let denied = shell_fixture(directory.path(), Decision::Deny).await;
    for (command, body) in [
        ("shell.openExternal", json!({ "url": url })),
        ("shell.openPath", json!({ "path": file.to_string_lossy() })),
        ("shell.trash", json!({ "path": file.to_string_lossy() })),
    ] {
        assert_eq!(
            denied.call(command, body).await.unwrap_err().code,
            ErrorCode::PermissionDenied,
            "{command}"
        );
    }
    assert!(denied.shell.asked().is_empty());
}
