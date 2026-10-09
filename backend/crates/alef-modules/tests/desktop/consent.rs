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

// Shortcut tests deliberately exercise only the module contract, not OS registration.
use alef_core::{
    registry::window::{
        shortcut::{ShortcutCall, ShortcutToken},
        UiCall,
    },
    session::Resource,
};
use std::{future::Future, pin::Pin, time::Duration};

async fn bounded<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(5), future)
        .await
        .expect("shortcut operation must complete")
}

async fn shortcuts(decision: Decision) -> Fixture {
    let text = MANIFEST.replace("global: false", "global: true");
    bounded(Fixture::new(Some(&text), &[]))
        .await
        .with_consent(consent(&[(Right::plain("shortcut.global"), decision)]))
}

fn queue(f: &Fixture, reply: Value) {
    f.host.replies.lock().unwrap().push_back(Ok(reply));
}

fn seen(f: &Fixture) -> Vec<(u64, UiCall)> {
    f.host.calls.lock().unwrap().clone()
}

async fn register_shortcut(f: &Fixture) -> Value {
    let reply = bounded(f.call(
        "shortcut.register",
        json!({"accelerator": "CommandOrControl+Shift+K"}),
    ))
    .await
    .unwrap();
    reply["id"].clone()
}

#[tokio::test]
async fn shortcut_denied_and_not_declared_never_ask_host() {
    let denied = shortcuts(Decision::Deny).await;
    let absent = bounded(Fixture::new(None, &[])).await;
    for f in [&denied, &absent] {
        assert_eq!(
            bounded(f.call("shortcut.register", json!({"accelerator": "Ctrl+K"})))
                .await
                .unwrap_err()
                .code,
            ErrorCode::PermissionDenied
        );
        assert!(seen(f).is_empty());
        assert!(f.session().resources().is_empty());
    }
}

#[tokio::test]
async fn shortcut_substitute_is_inert_but_validates_strictly() {
    let f = shortcuts(Decision::Substitute).await;
    for accelerator in [
        "",
        "Ctrl++K",
        "Ctrl+",
        "Ctrl+Shift",
        "Ctrl+K+Shift",
        "Bogus+K",
        "Ctrl+K\n",
        "Ctrl+\u{7f}K",
        &"K".repeat(257),
        // Valid, and one modifier too long: only the length refuses it.
        &format!("{}K", "Ctrl+".repeat(52)),
    ] {
        assert_eq!(
            bounded(f.call("shortcut.register", json!({"accelerator": accelerator})))
                .await
                .unwrap_err()
                .code,
            ErrorCode::InvalidArgument,
            "{accelerator:?}"
        );
    }
    for body in [
        Value::Null,
        json!({}),
        json!({"accelerator": 42}),
        json!({"accelerator": "Ctrl+K", "owner": 2}),
    ] {
        assert_eq!(
            bounded(f.call("shortcut.register", body))
                .await
                .unwrap_err()
                .code,
            ErrorCode::InvalidArgument
        );
    }
    assert_eq!(
        bounded(f.call_reply(
            "shortcut.register",
            json!({"accelerator": "Ctrl+K"}),
            Some(bytes::Bytes::from_static(b"x"))
        ))
        .await
        .unwrap_err()
        .code,
        ErrorCode::InvalidArgument
    );
    // The longest accepted accelerator is 256 bytes.
    bounded(f.call(
        "shortcut.register",
        json!({"accelerator": format!("{}K", "Ctrl+".repeat(51))}),
    ))
    .await
    .unwrap();
    assert_eq!(f.session().resources().len(), 1);
    let reply = bounded(f.call(
        "shortcut.register",
        json!({"accelerator": "CommandOrControl+Shift+K"}),
    ))
    .await
    .unwrap();
    let id = reply["id"].clone();
    assert_eq!(
        reply,
        json!({"id": id, "owner": f.session().id().0, "token": null})
    );
    // Neither a body nor a handle that is not exactly the one given releases it.
    assert_eq!(
        bounded(f.call_reply(
            "shortcut.unregister",
            json!({"id": id}),
            Some(bytes::Bytes::from_static(b"x"))
        ))
        .await
        .unwrap_err()
        .code,
        ErrorCode::InvalidArgument
    );
    let text = id.as_str().unwrap().to_owned();
    let (session, resource) = text.strip_prefix('s').unwrap().split_once(":r").unwrap();
    for forged in [
        format!("s{}:r{resource}", session.parse::<u64>().unwrap() + 1),
        format!("s{session}:r+{resource}"),
        format!("s{session}:r0{resource}"),
        format!("s0{session}:r{resource}"),
        format!("s{session}:r0"),
        format!("{session}:r{resource}"),
        format!("s{session}:{resource}"),
    ] {
        assert_eq!(
            bounded(f.call("shortcut.unregister", json!({"id": forged})))
                .await
                .unwrap_err()
                .code,
            ErrorCode::NotFound,
            "{forged}"
        );
    }
    assert!(id.is_string());
    assert_eq!(f.session().resources().len(), 2);
    assert_eq!(
        bounded(f.call("shortcut.unregister", json!({"id": id})))
            .await
            .unwrap(),
        Value::Null
    );
    assert!(seen(&f).is_empty());
    assert_eq!(f.session().resources().len(), 1);
}

#[tokio::test]
async fn shortcut_real_unregister_and_reload_use_original_owner_and_token() {
    let mut f = shortcuts(Decision::Allow).await;
    let owner = f.session().id();
    queue(&f, json!(17));
    let reply = bounded(f.call(
        "shortcut.register",
        json!({"accelerator": "CommandOrControl+Shift+K"}),
    ))
    .await
    .unwrap();
    let id = reply["id"].clone();
    assert_eq!(reply, json!({"id": id, "owner": owner.0, "token": 17}));
    assert_eq!(
        seen(&f),
        vec![(
            1,
            UiCall::Shortcut(ShortcutCall::Register {
                owner,
                accelerator: "CommandOrControl+Shift+K".into(),
            })
        )]
    );
    let other = bounded(f.open_window(2)).await;
    assert_eq!(
        bounded(f.call_as(&other, "shortcut.unregister", json!({"id": id})))
            .await
            .unwrap_err()
            .code,
        ErrorCode::NotFound
    );
    assert_eq!(seen(&f).len(), 1);
    bounded(f.call("shortcut.unregister", json!({"id": id})))
        .await
        .unwrap();
    assert_eq!(
        seen(&f)[1],
        (
            1,
            UiCall::Shortcut(ShortcutCall::Unregister {
                owner,
                token: ShortcutToken(17)
            })
        )
    );
    assert_eq!(
        bounded(f.call("shortcut.unregister", json!({"id": id})))
            .await
            .unwrap_err()
            .code,
        ErrorCode::NotFound
    );
    queue(&f, json!(18));
    let stale = register_shortcut(&f).await;
    bounded(f.reload_document()).await;
    assert_eq!(
        seen(&f).last().unwrap(),
        &(
            1,
            UiCall::Shortcut(ShortcutCall::Unregister {
                owner,
                token: ShortcutToken(18)
            })
        )
    );
    queue(&f, json!(19));
    let fresh = register_shortcut(&f).await;
    assert_ne!(stale, fresh);
    assert_eq!(
        bounded(f.call("shortcut.unregister", json!({"id": stale})))
            .await
            .unwrap_err()
            .code,
        ErrorCode::NotFound
    );
    bounded(f.session().close()).await;
    assert_eq!(
        seen(&f).last().unwrap(),
        &(
            1,
            UiCall::Shortcut(ShortcutCall::Unregister {
                owner: f.session().id(),
                token: ShortcutToken(19),
            })
        )
    );
}

struct NotShortcut;
impl Resource for NotShortcut {
    fn close(self: Box<Self>) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        Box::pin(async {})
    }
}

#[tokio::test]
async fn shortcut_unregister_needs_no_permission_but_checks_type_and_canonical_handle() {
    let f = shortcuts(Decision::Deny).await;
    let resource = f
        .session()
        .resources()
        .insert(Box::new(NotShortcut))
        .unwrap();
    let owner = f.session().id().0;
    for id in [
        format!("s{owner}:r{}", resource.0),
        format!("s0{owner}:r1"),
        format!("s{owner}:r+1"),
        format!("s{owner}:r0"),
        "token".into(),
    ] {
        assert_eq!(
            bounded(f.call("shortcut.unregister", json!({"id": id})))
                .await
                .unwrap_err()
                .code,
            ErrorCode::NotFound
        );
    }
    assert_eq!(f.session().resources().len(), 1);
    for body in [json!({"id": 1}), json!({"id": "s1:r1", "extra": true})] {
        assert_eq!(
            bounded(f.call("shortcut.unregister", body))
                .await
                .unwrap_err()
                .code,
            ErrorCode::InvalidArgument
        );
    }
    assert!(seen(&f).is_empty());
}

#[tokio::test]
async fn shortcut_invalid_host_tokens_do_not_insert_resources() {
    let f = shortcuts(Decision::Allow).await;
    for reply in [
        Value::Null,
        json!(0),
        json!(-1),
        json!(1.5),
        json!(0xC000),
        json!(u64::MAX),
        json!("17"),
        json!({"token": 17}),
    ] {
        queue(&f, reply);
        assert_eq!(
            bounded(f.call("shortcut.register", json!({"accelerator": "Ctrl+K"})))
                .await
                .unwrap_err()
                .code,
            ErrorCode::Internal
        );
        assert!(f.session().resources().is_empty());
    }
    assert_eq!(seen(&f).len(), 8);
}

#[tokio::test]
async fn shortcut_insertion_failure_rolls_native_registration_back() {
    let f = shortcuts(Decision::Allow).await;
    let session = f.session();
    for _ in 0..session.resources().limit() {
        session.resources().insert(Box::new(NotShortcut)).unwrap();
    }
    queue(&f, json!(23));
    assert_eq!(
        bounded(f.call("shortcut.register", json!({"accelerator": "Ctrl+K"})))
            .await
            .unwrap_err()
            .code,
        ErrorCode::Busy
    );
    assert_eq!(
        seen(&f),
        vec![
            (
                1,
                UiCall::Shortcut(ShortcutCall::Register {
                    owner: session.id(),
                    accelerator: "Ctrl+K".into()
                })
            ),
            (
                1,
                UiCall::Shortcut(ShortcutCall::Unregister {
                    owner: session.id(),
                    token: ShortcutToken(23)
                })
            ),
        ]
    );
    assert_eq!(session.resources().len(), session.resources().limit());
}

#[tokio::test]
async fn shortcut_permission_revocation_does_not_prevent_unregister() {
    let f = shortcuts(Decision::Allow).await;
    queue(&f, json!(29));
    let id = register_shortcut(&f).await;
    let f = f.with_consent(consent(&[(
        Right::plain("shortcut.global"),
        Decision::Deny,
    )]));
    bounded(f.call("shortcut.unregister", json!({"id": id})))
        .await
        .unwrap();
    assert!(f.session().resources().is_empty());
    assert_eq!(seen(&f).len(), 2);
}

#[tokio::test]
async fn shortcut_substitute_reload_and_limit_never_ask_host() {
    let mut f = shortcuts(Decision::Substitute).await;
    let stale = register_shortcut(&f).await;
    let old = f.session();
    bounded(f.reload_document()).await;
    assert!(old.resources().is_empty());
    let fresh = register_shortcut(&f).await;
    assert_ne!(stale, fresh);
    assert_eq!(
        bounded(f.call("shortcut.unregister", json!({"id": stale})))
            .await
            .unwrap_err()
            .code,
        ErrorCode::NotFound
    );
    let session = f.session();
    for _ in 1..session.resources().limit() {
        session.resources().insert(Box::new(NotShortcut)).unwrap();
    }
    assert_eq!(
        bounded(f.call("shortcut.register", json!({"accelerator": "Ctrl+K"})))
            .await
            .unwrap_err()
            .code,
        ErrorCode::Busy
    );
    bounded(session.close()).await;
    assert!(seen(&f).is_empty());
}

#[tokio::test]
async fn shortcut_host_failure_keeps_resource_for_retry() {
    let f = shortcuts(Decision::Allow).await;
    queue(&f, json!(31));
    let id = register_shortcut(&f).await;
    f.host
        .replies
        .lock()
        .unwrap()
        .push_back(Err(alef_core::AlefError::new(
            ErrorCode::Internal,
            "host failed",
        )));
    assert_eq!(
        bounded(f.call("shortcut.unregister", json!({"id": id})))
            .await
            .unwrap_err()
            .code,
        ErrorCode::Internal
    );
    assert_eq!(f.session().resources().len(), 1);
    bounded(f.call("shortcut.unregister", json!({"id": id})))
        .await
        .unwrap();
    assert!(f.session().resources().is_empty());
    let calls = seen(&f);
    assert_eq!(calls[1], calls[2]);
}

#[tokio::test]
async fn shortcut_cancelled_registration_finishes_and_rolls_back_after_session_close() {
    let f = shortcuts(Decision::Allow).await;
    queue(&f, json!(37));
    let session = f.session();
    let context = alef_core::registry::context::CallContext::new(session.clone(), f.permissions());
    {
        let dispatch = f.registry.dispatch(
            "shortcut.register",
            context,
            json!({"accelerator": "Ctrl+K"}),
        );
        tokio::pin!(dispatch);
        let waker = std::task::Waker::noop();
        let mut task = std::task::Context::from_waker(waker);
        assert!(dispatch.as_mut().poll(&mut task).is_pending());
        // Dropping the transport's handler must not lose a native token.
    }
    bounded(session.close()).await;
    bounded(async {
        while seen(&f).len() < 2 {
            tokio::task::yield_now().await;
        }
    })
    .await;
    assert_eq!(
        seen(&f),
        vec![
            (
                1,
                UiCall::Shortcut(ShortcutCall::Register {
                    owner: session.id(),
                    accelerator: "Ctrl+K".into()
                })
            ),
            (
                1,
                UiCall::Shortcut(ShortcutCall::Unregister {
                    owner: session.id(),
                    token: ShortcutToken(37)
                })
            ),
        ]
    );
    assert!(session.resources().is_empty());
}
