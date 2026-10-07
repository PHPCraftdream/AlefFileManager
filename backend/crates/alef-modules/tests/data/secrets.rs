// SPDX-License-Identifier: MIT OR Apache-2.0
//! `secrets` through the registry: what is kept comes back as it went, every application has a
//! namespace of its own, the right is asked for and a stand-in keeps the secrets of a document apart.
//! The last test uses the real credential store and runs only on request (`ALEF_TEST_DESKTOP=1 cargo
//! test -p alef-modules --test data -- --ignored secrets`; CI does on its clean runners).
use std::sync::Arc;

use alef_core::{
    registry::{command::Reply, dispatch::Registry},
    security::{
        consent::{Consent, Decision, Right},
        permissions::Permission,
    },
    AlefError, ErrorCode,
};
use alef_modules::{register_all, MemorySecrets, SecretsBackend, SystemSecrets};
use bytes::Bytes;
use serde_json::{json, Value};

use crate::common::{desktop_asked, Fixture};

const MANIFEST: &str = include_str!("../fixtures/app.ktav");

/// The manifest of the application `id`, asking for the right to keep secrets or not.
fn manifest(id: &str, asking: bool) -> String {
    let text = MANIFEST
        .replace('\r', "")
        .replace("id: org.example.modules", &format!("id: {id}"));
    let text = if asking {
        text.replace("secrets: false", "secrets: true")
    } else {
        text
    };
    assert!(text.contains(&format!("id: {id}")));
    assert_eq!(text.contains("secrets: true"), asking);
    text
}

/// The application `id` on a machine whose store of secrets is `secrets`.
async fn app_on(id: &str, asking: bool, secrets: &Arc<MemorySecrets>) -> Fixture {
    let root = tempfile::tempdir().unwrap();
    Fixture::new_sharing(
        root.path(),
        Some(&manifest(id, asking)),
        &[],
        secrets.clone(),
    )
    .await
}

async fn app() -> Fixture {
    app_on("org.example.modules", true, &Arc::default()).await
}

fn names(service: &str, account: &str) -> Value {
    json!({ "service": service, "account": account })
}

async fn set(
    app: &Fixture,
    service: &str,
    account: &str,
    secret: &[u8],
) -> Result<Reply, AlefError> {
    app.call_reply(
        "secrets.set",
        names(service, account),
        Some(Bytes::copy_from_slice(secret)),
    )
    .await
}

async fn get(app: &Fixture, service: &str, account: &str) -> Option<Vec<u8>> {
    match app
        .call_reply("secrets.get", names(service, account), None)
        .await
        .expect("get")
    {
        Reply::Bytes(bytes) => Some(bytes.to_vec()),
        Reply::Json(Value::Null) => None,
        other => panic!("expected bytes or null, got {other:?}"),
    }
}

async fn delete(app: &Fixture, service: &str, account: &str) -> bool {
    let reply = app
        .call("secrets.delete", names(service, account))
        .await
        .expect("delete");
    reply["deleted"].as_bool().expect("deleted is a boolean")
}

#[tokio::test]
async fn a_secret_comes_back_as_it_went_and_what_is_not_kept_is_not_an_empty_one() {
    let app = app().await;
    assert_eq!(get(&app, "mail", "me").await, None);

    let every_byte: Vec<u8> = (0..=255).collect();
    set(&app, "mail", "me", &every_byte).await.expect("set");
    assert_eq!(get(&app, "mail", "me").await, Some(every_byte));
    let longest = vec![0xA5; 1024];
    set(&app, "mail", "me", &longest)
        .await
        .expect("the longest");
    assert_eq!(
        get(&app, "mail", "me").await,
        Some(longest),
        "a second set replaces the first"
    );

    set(&app, "mail", "you", b"another account").await.unwrap();
    set(&app, "chat", "me", b"another service").await.unwrap();
    assert_eq!(get(&app, "mail", "you").await.unwrap(), b"another account");
    assert_eq!(get(&app, "chat", "me").await.unwrap(), b"another service");
    assert_eq!(get(&app, "mail", "me").await.unwrap().len(), 1024);

    let (service, account) = ("сервис — 🙂", "שלום@example.com");
    set(&app, service, account, "пароль".as_bytes())
        .await
        .unwrap();
    assert_eq!(
        get(&app, service, account).await.unwrap(),
        "пароль".as_bytes()
    );

    assert!(delete(&app, "mail", "me").await);
    assert_eq!(get(&app, "mail", "me").await, None, "deleted");
    assert!(!delete(&app, "mail", "me").await, "nothing to delete");
    assert!(get(&app, "mail", "you").await.is_some(), "the others stay");
}

#[tokio::test]
async fn a_secret_has_from_1_to_1024_bytes_and_the_names_are_plain() {
    let app = app().await;
    let refused = |result: Result<Reply, AlefError>, what: &str| {
        let error = result.expect_err(what);
        assert_eq!(error.code, ErrorCode::InvalidArgument, "{what}");
    };
    refused(set(&app, "mail", "me", b"").await, "an empty secret");
    refused(set(&app, "mail", "me", &[1; 1025]).await, "1025 bytes");
    refused(
        app.call_reply("secrets.set", names("mail", "me"), None)
            .await,
        "no body",
    );

    let long = "x".repeat(129);
    for (service, account, what) in [
        ("", "me", "an empty service"),
        ("mail", "", "an empty account"),
        (long.as_str(), "me", "a long service"),
        ("mail", long.as_str(), "a long account"),
        ("ma\nil", "me", "a service with a line break"),
        ("mail", "m\u{0}e", "an account with a null"),
        ("ma\u{7f}il", "me", "a service with DEL"),
        ("mail", "m\u{85}e", "an account with a C1 control"),
    ] {
        refused(set(&app, service, account, b"secret").await, what);
        for command in ["secrets.get", "secrets.delete"] {
            refused(
                app.call_reply(command, names(service, account), None).await,
                what,
            );
        }
    }
    refused(
        app.call_reply(
            "secrets.get",
            json!({ "service": "mail", "account": "me", "extra": 1 }),
            None,
        )
        .await,
        "an argument that is not known",
    );
    assert!(
        app.secrets.names().is_empty(),
        "nothing refused was kept: {:?}",
        app.secrets.names()
    );

    let widest = "ж".repeat(64);
    set(&app, &widest, &widest, b"128 bytes each")
        .await
        .expect("128 bytes");
    assert_eq!(
        get(&app, &widest, &widest).await.unwrap(),
        b"128 bytes each"
    );
    set(&app, "mail", "me", &[1]).await.expect("one byte");
}

#[tokio::test]
async fn every_application_has_a_namespace_of_its_own() {
    let machine = Arc::new(MemorySecrets::default());
    let one = app_on("org.example.one", true, &machine).await;
    let two = app_on("org.example.two", true, &machine).await;
    set(&one, "mail", "me", b"first").await.unwrap();
    assert_eq!(
        get(&two, "mail", "me").await,
        None,
        "the other does not see it"
    );
    set(&two, "mail", "me", b"second").await.unwrap();
    assert_eq!(get(&one, "mail", "me").await.unwrap(), b"first");
    assert_eq!(get(&two, "mail", "me").await.unwrap(), b"second");
    assert_eq!(
        machine.names(),
        [
            ("alef/org.example.one/mail".to_owned(), "me".to_owned()),
            ("alef/org.example.two/mail".to_owned(), "me".to_owned()),
        ]
    );
    assert!(delete(&one, "mail", "me").await);
    assert_eq!(
        get(&two, "mail", "me").await.unwrap(),
        b"second",
        "a delete is its own"
    );
    assert!(!delete(&one, "mail", "me").await);

    // An id with a / would reach into the namespace of another application.
    let mut context = one.context.clone();
    context.app.id = "org.example.one/mail".to_owned();
    let refused =
        register_all(&mut Registry::default(), one.host.clone(), &context).expect_err("such an id");
    assert_eq!(refused.code, ErrorCode::InvalidArgument);
    context.app.id = String::new();
    assert!(register_all(&mut Registry::default(), one.host.clone(), &context).is_err());
}

fn deciding(decision: Decision) -> Consent {
    let mut consent = Consent::undecided();
    consent.set(Right::plain("secrets"), decision);
    consent
}

#[tokio::test]
async fn the_right_is_asked_for_and_a_denied_one_keeps_every_command_out() {
    let machine = Arc::new(MemorySecrets::default());
    let silent = app_on("org.example.modules", false, &machine).await;
    for command in ["secrets.get", "secrets.set", "secrets.delete"] {
        let error = silent
            .call_reply(command, names("mail", "me"), Some(Bytes::from_static(b"x")))
            .await
            .expect_err(command);
        assert_eq!(error.code, ErrorCode::PermissionDenied, "{command}");
        assert_eq!(
            error.details,
            Some(json!({ "permission": Permission::Secrets.name() })),
            "{command}"
        );
    }
    let denied = app_on("org.example.modules", true, &machine)
        .await
        .with_consent(deciding(Decision::Deny));
    let error = set(&denied, "mail", "me", b"x").await.unwrap_err();
    assert_eq!(error.code, ErrorCode::PermissionDenied, "the user said no");
    assert!(machine.names().is_empty());

    let allowed = app_on("org.example.modules", true, &machine)
        .await
        .with_consent(deciding(Decision::Allow));
    set(&allowed, "mail", "me", b"x").await.expect("allowed");
    assert_eq!(machine.names().len(), 1);
}

#[tokio::test]
async fn a_stand_in_keeps_the_secrets_of_a_document_apart_from_the_real_ones() {
    let machine = Arc::new(MemorySecrets::default());
    let real = app_on("org.example.modules", true, &machine)
        .await
        .with_consent(deciding(Decision::Allow));
    set(&real, "mail", "me", b"real").await.unwrap();

    let pretending = app_on("org.example.modules", true, &machine)
        .await
        .with_consent(deciding(Decision::Substitute));
    assert_eq!(
        get(&pretending, "mail", "me").await,
        None,
        "the stand-in does not show the real secret"
    );
    set(&pretending, "mail", "me", b"made up").await.unwrap();
    set(&pretending, "chat", "you", b"also made up")
        .await
        .unwrap();
    assert_eq!(get(&pretending, "mail", "me").await.unwrap(), b"made up");
    assert_eq!(
        get(&real, "mail", "me").await.unwrap(),
        b"real",
        "the real one is untouched"
    );
    assert_eq!(get(&real, "chat", "you").await, None);
    assert!(delete(&pretending, "mail", "me").await);
    assert!(!delete(&pretending, "mail", "me").await);
    assert_eq!(
        get(&real, "mail", "me").await.unwrap(),
        b"real",
        "so is it after a delete"
    );
    assert_eq!(machine.names().len(), 1);

    // The stand-in is of the run, not of the machine: another document starts with nothing.
    let next = app_on("org.example.modules", true, &machine)
        .await
        .with_consent(deciding(Decision::Substitute));
    assert_eq!(get(&next, "chat", "you").await, None);
}

#[test]
fn the_memory_store_is_what_a_pretending_run_gives_and_never_shows_what_it_keeps() {
    let backends = alef_modules::Backends::pretending(None, None);
    backends.secrets.set("service", "account", b"hush").unwrap();
    assert_eq!(
        backends.secrets.get("service", "account").unwrap().unwrap(),
        b"hush"
    );
    assert!(backends.secrets.delete("service", "account").unwrap());
    assert!(backends
        .secrets
        .get("service", "account")
        .unwrap()
        .is_none());

    let memory = MemorySecrets::default();
    memory.set("a-service", "an-account", b"a-secret").unwrap();
    let shown = format!(
        "{memory:?} {:?} {:?}",
        backends.secrets,
        SystemSecrets::default()
    );
    for hidden in ["a-service", "an-account", "a-secret"] {
        assert!(!shown.contains(hidden), "{shown}");
    }
}

#[test]
#[ignore = "uses the credential store of the desktop"]
fn the_system_store_keeps_secrets_and_forgets_them() {
    if !desktop_asked() {
        eprintln!("skipped: ALEF_TEST_DESKTOP=1 allows the test to use the credential store");
        return;
    }
    let store = SystemSecrets::default();
    let unique = format!(
        "alef-test/{}-{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos()
    );
    let (dotted, plain) = (format!("{unique}.a"), unique.clone());
    let cleanup = |store: &SystemSecrets| {
        for (service, account) in [(&unique, "me"), (&dotted, "b"), (&plain, "a.b")] {
            let _ = store.delete(service, account);
        }
    };
    cleanup(&store);

    assert_eq!(store.get(&unique, "me").expect("get"), None);
    assert!(!store.delete(&unique, "me").expect("delete of nothing"));

    let binary: Vec<u8> = (0..=255).cycle().take(1024).collect();
    store.set(&unique, "me", &binary).expect("set");
    assert_eq!(store.get(&unique, "me").expect("get"), Some(binary));
    store.set(&unique, "me", b"replaced").expect("set again");
    assert_eq!(store.get(&unique, "me").unwrap().unwrap(), b"replaced");

    // The name of a credential must not depend on where a dot lies between service and account.
    store.set(&dotted, "b", b"one").expect("a dotted service");
    assert_eq!(
        store.get(&plain, "a.b").expect("get"),
        None,
        "(service.a, b) is not (service, a.b)"
    );
    store.set(&plain, "a.b", b"two").expect("a dotted account");
    assert_eq!(store.get(&dotted, "b").unwrap().unwrap(), b"one");
    assert_eq!(store.get(&plain, "a.b").unwrap().unwrap(), b"two");

    assert!(store.delete(&unique, "me").expect("delete"));
    assert_eq!(store.get(&unique, "me").unwrap(), None);
    cleanup(&store);
}
