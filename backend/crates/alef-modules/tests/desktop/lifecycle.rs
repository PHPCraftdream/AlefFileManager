// SPDX-License-Identifier: MIT OR Apache-2.0
//! The life of the application through the registry: the single instance and the question the
//! documents are asked before it quits.

use std::{sync::Arc, time::Duration};

use crate::common::Fixture;
use alef_core::{session::Session, ErrorCode};
use serde_json::{json, Value};

const MANIFEST: &str = include_str!("../fixtures/app.ktav");

/// The fixture manifest of an application of its own: instances of one application meet, others do not.
fn manifest_of(id: &str) -> String {
    let text = MANIFEST.replace('\r', "");
    let named = text.replacen("id: org.example.modules", &format!("id: {id}"), 1);
    assert_ne!(named, text, "the fixture names its application");
    named
}

async fn instance(id: &str, command_line: &[&str]) -> Fixture {
    Fixture::new(Some(&manifest_of(id)), command_line).await
}

type Event = (Option<u64>, String, Value);

fn events(fixture: &Fixture) -> Vec<Event> {
    fixture.host.events.lock().unwrap().clone()
}

/// Waits (a few seconds at most) until the host has been sent `count` events.
async fn events_after(fixture: &Fixture, count: usize) -> Vec<Event> {
    for _ in 0..500 {
        let seen = events(fixture);
        if seen.len() >= count {
            return seen;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("expected {count} events, got {:?}", events(fixture));
}

#[tokio::test]
async fn the_first_instance_is_told_so_and_hears_of_a_later_one() {
    let first = instance("org.example.lifecycle.first", &["--port", "1", "a.txt"]).await;
    let later = instance(
        "org.example.lifecycle.first",
        &["--port", "2", "b.txt", "c.txt"],
    )
    .await;
    assert_eq!(
        first
            .call("app.requestSingleInstance", Value::Null)
            .await
            .unwrap(),
        json!(true)
    );
    assert_eq!(
        first
            .call("app.requestSingleInstance", Value::Null)
            .await
            .unwrap(),
        json!(true),
        "asking again does not change the answer"
    );
    assert_eq!(
        later
            .call("app.requestSingleInstance", Value::Null)
            .await
            .unwrap(),
        json!(false)
    );
    assert_eq!(
        later
            .call("app.requestSingleInstance", Value::Null)
            .await
            .unwrap(),
        json!(false)
    );
    let seen = events_after(&first, 1).await;
    assert_eq!(
        seen.len(),
        1,
        "the later instance was announced once: {seen:?}"
    );
    let (window, name, payload) = &seen[0];
    assert_eq!(*window, None, "every window hears of it");
    assert_eq!(name, "app.second-instance");
    assert_eq!(payload["args"]["positional"], json!(["b.txt", "c.txt"]));
    assert_eq!(payload["args"]["parsed"]["port"], json!(2.0));
    assert_eq!(
        payload["args"]["raw"],
        json!(["--port", "2", "b.txt", "c.txt"])
    );
    assert_eq!(
        payload["cwd"],
        json!(std::env::current_dir().unwrap().to_string_lossy())
    );
    assert!(
        events(&later).is_empty(),
        "the later instance hears nothing"
    );
}

#[tokio::test]
async fn every_later_instance_is_announced_and_other_applications_do_not_meet() {
    let first = instance("org.example.lifecycle.many", &["a.txt"]).await;
    assert_eq!(
        first
            .call("app.requestSingleInstance", Value::Null)
            .await
            .unwrap(),
        json!(true)
    );
    for file in ["b.txt", "c.txt", "d.txt"] {
        let later = instance("org.example.lifecycle.many", &[file]).await;
        assert_eq!(
            later
                .call("app.requestSingleInstance", Value::Null)
                .await
                .unwrap(),
            json!(false)
        );
    }
    let seen = events_after(&first, 3).await;
    let mut files: Vec<String> = seen
        .iter()
        .map(|(_, _, payload)| {
            payload["args"]["positional"][0]
                .as_str()
                .unwrap()
                .to_owned()
        })
        .collect();
    files.sort();
    assert_eq!(files, ["b.txt", "c.txt", "d.txt"]);

    let other = instance("org.example.lifecycle.other", &[]).await;
    assert_eq!(
        other
            .call("app.requestSingleInstance", Value::Null)
            .await
            .unwrap(),
        json!(true),
        "another application is the first of its own"
    );
    assert_eq!(
        events(&first).len(),
        3,
        "and the first one heard nothing of it"
    );
}

#[tokio::test]
async fn an_application_that_never_asks_is_not_in_the_way() {
    let quiet = instance("org.example.lifecycle.quiet", &[]).await;
    let asking = instance("org.example.lifecycle.quiet", &[]).await;
    assert_eq!(
        asking
            .call("app.requestSingleInstance", Value::Null)
            .await
            .unwrap(),
        json!(true)
    );
    assert!(events(&quiet).is_empty());
}

fn quit_calls(fixture: &Fixture) -> Vec<i32> {
    fixture.host.quits.lock().unwrap().clone()
}

async fn asked(fixture: &Fixture, session: &Arc<Session>) {
    fixture
        .call_as(session, "app.quitIntercept", json!({ "enabled": true }))
        .await
        .expect("intercept");
}

fn before_quit(fixture: &Fixture) -> Vec<(Option<u64>, u64)> {
    events(fixture)
        .into_iter()
        .filter(|(_, name, _)| name == "app.before-quit")
        .map(|(window, _, payload)| (window, payload["id"].as_u64().unwrap()))
        .collect()
}

#[tokio::test]
async fn quitting_with_nobody_to_ask_quits_at_once() {
    let fixture = Fixture::new(None, &[]).await;
    fixture
        .call("app.quit", json!({ "code": 3 }))
        .await
        .expect("quit");
    assert_eq!(quit_calls(&fixture), [3]);
    assert!(events(&fixture).is_empty());
}

#[tokio::test]
async fn a_document_that_allows_lets_the_application_quit() {
    let fixture = Arc::new(Fixture::new(None, &[]).await);
    let main = fixture.session();
    asked(&fixture, &main).await;
    let quitting = {
        let fixture = fixture.clone();
        tokio::spawn(async move { fixture.call("app.quit", json!({ "code": 4 })).await })
    };
    let round = events_after(&fixture, 1).await;
    assert_eq!(
        round[0].0,
        Some(main.window()),
        "the question goes to the window that asked to be asked"
    );
    assert!(
        quit_calls(&fixture).is_empty(),
        "nothing quits before the answer"
    );
    let id = before_quit(&fixture)[0].1;
    fixture
        .call_as(
            &main,
            "app.quitAnswer",
            json!({ "id": id, "prevent": false }),
        )
        .await
        .expect("answer");
    quitting.await.unwrap().expect("quit");
    assert_eq!(quit_calls(&fixture), [4]);
}

#[tokio::test]
async fn a_document_that_vetoes_keeps_the_application_running() {
    let fixture = Arc::new(Fixture::new(None, &[]).await);
    let main = fixture.session();
    asked(&fixture, &main).await;
    let quitting = {
        let fixture = fixture.clone();
        tokio::spawn(async move { fixture.call("app.quit", json!({ "code": 5 })).await })
    };
    events_after(&fixture, 1).await;
    let id = before_quit(&fixture)[0].1;
    fixture
        .call_as(
            &main,
            "app.quitAnswer",
            json!({ "id": id, "prevent": true }),
        )
        .await
        .expect("answer");
    quitting.await.unwrap().expect("the call itself succeeds");
    assert!(quit_calls(&fixture).is_empty(), "the veto stands");
    // The next quit asks again.
    let again = {
        let fixture = fixture.clone();
        tokio::spawn(async move { fixture.call("app.quit", json!({})).await })
    };
    events_after(&fixture, 2).await;
    let second = before_quit(&fixture)[1].1;
    assert_ne!(second, id, "every question has its own number");
    fixture
        .call_as(
            &main,
            "app.quitAnswer",
            json!({ "id": second, "prevent": false }),
        )
        .await
        .unwrap();
    again.await.unwrap().unwrap();
    assert_eq!(quit_calls(&fixture), [0]);
}

#[tokio::test(start_paused = true)]
async fn silence_allows_the_quit_after_the_limit() {
    let fixture = Arc::new(Fixture::new(None, &[]).await);
    asked(&fixture, &fixture.session()).await;
    let quitting = {
        let fixture = fixture.clone();
        tokio::spawn(async move { fixture.call("app.quit", json!({ "code": 6 })).await })
    };
    tokio::time::sleep(Duration::from_millis(2900)).await;
    assert!(
        quit_calls(&fixture).is_empty(),
        "the document still has time"
    );
    tokio::time::sleep(Duration::from_millis(300)).await;
    quitting.await.unwrap().expect("quit");
    assert_eq!(quit_calls(&fixture), [6]);
}

#[tokio::test]
async fn every_window_that_asked_is_asked_and_one_veto_is_enough() {
    let fixture = Arc::new(Fixture::new(None, &[]).await);
    let main = fixture.session();
    let tool = fixture.open_window(2).await;
    asked(&fixture, &main).await;
    asked(&fixture, &tool).await;
    let quitting = {
        let fixture = fixture.clone();
        tokio::spawn(async move { fixture.call("app.quit", json!({ "code": 7 })).await })
    };
    events_after(&fixture, 2).await;
    let mut windows: Vec<_> = before_quit(&fixture)
        .iter()
        .map(|(window, _)| window.unwrap())
        .collect();
    windows.sort_unstable();
    assert_eq!(windows, [main.window(), tool.window()]);
    let id = before_quit(&fixture)[0].1;
    fixture
        .call_as(
            &main,
            "app.quitAnswer",
            json!({ "id": id, "prevent": false }),
        )
        .await
        .unwrap();
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert!(
        quit_calls(&fixture).is_empty(),
        "the other window has not answered"
    );
    fixture
        .call_as(
            &tool,
            "app.quitAnswer",
            json!({ "id": id, "prevent": true }),
        )
        .await
        .unwrap();
    quitting.await.unwrap().unwrap();
    assert!(quit_calls(&fixture).is_empty(), "one veto is enough");
}

#[tokio::test]
async fn a_veto_ends_the_round_without_waiting_for_the_others() {
    let fixture = Arc::new(Fixture::new(None, &[]).await);
    let main = fixture.session();
    let tool = fixture.open_window(2).await;
    asked(&fixture, &main).await;
    asked(&fixture, &tool).await;
    let quitting = {
        let fixture = fixture.clone();
        tokio::spawn(async move { fixture.call("app.quit", json!({})).await })
    };
    events_after(&fixture, 2).await;
    let id = before_quit(&fixture)[0].1;
    fixture
        .call_as(
            &tool,
            "app.quitAnswer",
            json!({ "id": id, "prevent": true }),
        )
        .await
        .unwrap();
    tokio::time::timeout(Duration::from_secs(1), quitting)
        .await
        .expect("the main window never answered and is not waited for")
        .unwrap()
        .unwrap();
    assert!(quit_calls(&fixture).is_empty());
}

#[tokio::test]
async fn answers_of_windows_that_were_not_asked_or_to_other_questions_change_nothing() {
    let fixture = Arc::new(Fixture::new(None, &[]).await);
    let main = fixture.session();
    let bystander = fixture.open_window(3).await;
    asked(&fixture, &main).await;
    let quitting = {
        let fixture = fixture.clone();
        tokio::spawn(async move { fixture.call("app.quit", json!({ "code": 8 })).await })
    };
    events_after(&fixture, 1).await;
    let id = before_quit(&fixture)[0].1;
    for (session, round) in [(&bystander, id), (&main, id + 1000)] {
        fixture
            .call_as(
                session,
                "app.quitAnswer",
                json!({ "id": round, "prevent": true }),
            )
            .await
            .expect("an answer that does not count is not an error");
    }
    assert!(
        quit_calls(&fixture).is_empty(),
        "still waiting for the window that was asked"
    );
    fixture
        .call_as(
            &main,
            "app.quitAnswer",
            json!({ "id": id, "prevent": false }),
        )
        .await
        .unwrap();
    quitting.await.unwrap().unwrap();
    assert_eq!(quit_calls(&fixture), [8], "the stray vetoes did not count");
    assert!(
        bystander.is_open(),
        "and the window that was not asked is left alone"
    );
}

#[tokio::test]
async fn a_document_that_stopped_asking_or_went_away_is_not_asked() {
    let mut fixture = Fixture::new(None, &[]).await;
    let main = fixture.session();
    asked(&fixture, &main).await;
    fixture
        .call_as(&main, "app.quitIntercept", json!({ "enabled": false }))
        .await
        .unwrap();
    fixture
        .call("app.quit", json!({ "code": 1 }))
        .await
        .unwrap();
    assert_eq!(quit_calls(&fixture), [1], "it stopped asking");
    assert!(events(&fixture).is_empty());

    asked(&fixture, &fixture.session()).await;
    fixture.reload_document().await;
    fixture
        .call("app.quit", json!({ "code": 2 }))
        .await
        .unwrap();
    assert_eq!(
        quit_calls(&fixture),
        [1, 2],
        "the document that asked is gone"
    );
    assert!(events(&fixture).is_empty(), "nobody was asked");
}

#[tokio::test]
async fn bad_arguments_are_refused() {
    let fixture = Fixture::new(None, &[]).await;
    for (command, body) in [
        ("app.quitIntercept", json!({})),
        ("app.quitIntercept", json!({ "enabled": "yes" })),
        ("app.quitAnswer", json!({ "id": 1 })),
        ("app.quitAnswer", json!({ "id": -1, "prevent": true })),
        (
            "app.quitAnswer",
            json!({ "id": 1, "prevent": true, "extra": 1 }),
        ),
        ("app.requestSingleInstance", json!({ "extra": 1 })),
    ] {
        let error = fixture
            .call(command, body.clone())
            .await
            .expect_err(command);
        assert_eq!(error.code, ErrorCode::InvalidArgument, "{command} {body}");
    }
    assert!(quit_calls(&fixture).is_empty());
}

#[tokio::test]
async fn a_signal_quits_with_its_code_when_nobody_is_asked_and_asks_the_documents_that_want_to_be()
{
    let fixture = Arc::new(Fixture::new(None, &[]).await);
    let termination = fixture.context.termination.clone();
    assert!(termination.request(143).await, "nobody to ask");
    assert_eq!(quit_calls(&fixture), [143]);

    let main = fixture.session();
    asked(&fixture, &main).await;
    let terminating = {
        let termination = termination.clone();
        tokio::spawn(async move { termination.request(130).await })
    };
    events_after(&fixture, 1).await;
    assert_eq!(
        quit_calls(&fixture),
        [143],
        "nothing quits before the answer"
    );
    let id = before_quit(&fixture)[0].1;
    fixture
        .call_as(
            &main,
            "app.quitAnswer",
            json!({ "id": id, "prevent": true }),
        )
        .await
        .expect("answer");
    assert!(!terminating.await.unwrap(), "a veto keeps the application");
    assert_eq!(quit_calls(&fixture), [143]);
}

#[tokio::test]
async fn a_signal_before_the_modules_are_registered_quits_nothing() {
    assert!(!alef_modules::Termination::default().request(143).await);
}
