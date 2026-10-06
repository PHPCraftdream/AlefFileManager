// SPDX-License-Identifier: MIT OR Apache-2.0
//! `window` and `screen` through the registry: arguments, permission, and what reaches the host.

use crate::common::Fixture;
use alef_core::{
    registry::window::{ResizeEdge, UiCall, WindowCall, WindowOp},
    security::window::{Length, LengthUnit},
    AlefError, ErrorCode,
};
use serde_json::{json, Value};

const MANIFEST: &str = include_str!("../fixtures/app.ktav");

fn allowing_create() -> String {
    let open = MANIFEST.replace('\r', "");
    let granted = open.replacen(
        "    app: {",
        "    window: {\n        create: true\n    }\n    app: {",
        1,
    );
    assert_ne!(
        granted, open,
        "the fixture has an app section to put the grant before"
    );
    granted
}

fn window(label: Option<&str>, op: WindowOp) -> UiCall {
    UiCall::Window(WindowCall {
        label: label.map(str::to_owned),
        op,
    })
}

async fn ask(fixture: &Fixture, command: &str, body: Value) -> Result<Value, AlefError> {
    fixture.call(command, body).await
}

fn calls(fixture: &Fixture) -> Vec<(u64, UiCall)> {
    fixture.host.calls.lock().unwrap().clone()
}

#[tokio::test]
async fn every_operation_is_a_command_and_reaches_the_host_with_the_calling_window() {
    let fixture = Fixture::new(None, &[]).await;
    let cases = [
        ("window.state", Value::Null, WindowOp::State),
        ("window.center", json!({}), WindowOp::Center),
        ("window.close", Value::Null, WindowOp::Close),
        (
            "window.setTitle",
            json!({"title": "Hello"}),
            WindowOp::SetTitle {
                title: "Hello".to_owned(),
            },
        ),
        (
            "window.setSize",
            json!({"width": 640, "height": "60%work"}),
            WindowOp::SetSize {
                width: Length::Px(640.0),
                height: Length::Percent(60.0, LengthUnit::Work),
            },
        ),
        (
            "window.setMinSize",
            json!({"width": 300}),
            WindowOp::SetMinSize {
                width: Some(Length::Px(300.0)),
                height: None,
            },
        ),
        (
            "window.startResize",
            json!({"edge": "northWest"}),
            WindowOp::StartResize {
                edge: ResizeEdge::NorthWest,
            },
        ),
        (
            "window.closeAnswer",
            json!({"id": 4, "prevent": true}),
            WindowOp::CloseAnswer {
                id: 4,
                prevent: true,
            },
        ),
    ];
    for (command, body, op) in &cases {
        ask(&fixture, command, body.clone()).await.expect(command);
        let seen = calls(&fixture).pop().expect("one call");
        assert_eq!(seen, (1, window(None, op.clone())), "{command}");
    }
    let before = calls(&fixture).len();
    ask(
        &fixture,
        "window.setTitle",
        json!({"label": "second", "title": "T"}),
    )
    .await
    .expect("with a label");
    let all = calls(&fixture);
    assert_eq!(all.len(), before + 1);
    assert_eq!(
        all.last().unwrap().1,
        window(
            Some("second"),
            WindowOp::SetTitle {
                title: "T".to_owned()
            }
        )
    );
}

#[tokio::test]
async fn the_command_decides_the_operation_not_the_body() {
    let fixture = Fixture::new(None, &[]).await;
    ask(
        &fixture,
        "window.close",
        json!({"op": "setTitle", "title": "x"}),
    )
    .await
    .expect("close");
    assert_eq!(calls(&fixture), [(1, window(None, WindowOp::Close))]);
}

#[tokio::test]
async fn the_reply_of_the_host_is_the_reply_of_the_command_and_its_errors_pass_through() {
    let fixture = Fixture::new(None, &[]).await;
    let info = json!({"label": "main", "title": "Modules"});
    {
        let mut replies = fixture.host.replies.lock().unwrap();
        replies.push_back(Ok(info.clone()));
        replies.push_back(Err(AlefError::new(ErrorCode::NotFound, "no such window")));
    }
    assert_eq!(
        ask(&fixture, "window.state", Value::Null).await.unwrap(),
        info
    );
    let error = ask(&fixture, "window.state", json!({"label": "gone"}))
        .await
        .expect_err("host error");
    assert_eq!(error.code, ErrorCode::NotFound);
}

#[tokio::test]
async fn bad_arguments_are_refused_before_the_host_hears_of_them() {
    let fixture = Fixture::new(None, &[]).await;
    for (command, body) in [
        ("window.setTitle", json!({})),
        ("window.setTitle", json!({"title": 5})),
        ("window.setSize", json!({"width": "wide", "height": 1})),
        ("window.setSize", json!({"width": 100})),
        ("window.setPosition", json!({"x": -1, "y": 0})),
        ("window.startResize", json!({"edge": "up"})),
        ("window.setFullscreen", json!({"enabled": "yes"})),
        ("window.setZoom", json!({"factor": "big"})),
        ("window.state", json!({"label": 7})),
        ("window.state", json!(5)),
        (
            "window.closeIntercept",
            json!({"enabled": true, "label": "other"}),
        ),
        (
            "window.closeAnswer",
            json!({"id": 1, "prevent": false, "label": "other"}),
        ),
    ] {
        let error = ask(&fixture, command, body.clone())
            .await
            .expect_err("must be refused");
        assert_eq!(error.code, ErrorCode::InvalidArgument, "{command} {body}");
    }
    assert!(calls(&fixture).is_empty());
}

#[tokio::test]
async fn creating_a_window_needs_the_manifest_permission_and_a_valid_definition() {
    let closed = Fixture::new(None, &[]).await;
    let definition = json!({"label": "tool", "url": "/tool.html", "width": 400, "height": 300});
    let error = ask(&closed, "window.create", definition.clone())
        .await
        .expect_err("no permission");
    assert_eq!(error.code, ErrorCode::PermissionDenied);
    let error = ask(&closed, "window.create", json!("not even an object"))
        .await
        .expect_err("permission comes first");
    assert_eq!(
        error.code,
        ErrorCode::PermissionDenied,
        "the arguments are not looked at"
    );
    assert!(calls(&closed).is_empty());

    let open = Fixture::new(Some(&allowing_create()), &[]).await;
    ask(&open, "window.create", definition)
        .await
        .expect("create");
    let seen = calls(&open);
    let [(1, UiCall::Create(created))] = &seen[..] else {
        panic!("expected one create call, got {seen:?}");
    };
    assert_eq!(
        (created.label.as_str(), created.url.as_str()),
        ("tool", "/tool.html")
    );
    assert_eq!(created.width, Length::Px(400.0));

    for (what, body) in [
        (
            "an empty label",
            json!({"label": " ", "url": "/", "width": 1, "height": 1}),
        ),
        (
            "another host",
            json!({"label": "a", "url": "//evil.example/", "width": 1, "height": 1}),
        ),
        (
            "a relative url",
            json!({"label": "a", "url": "x.html", "width": 1, "height": 1}),
        ),
        (
            "minimum above maximum",
            json!({"label": "a", "url": "/", "width": 9, "height": 9, "minWidth": 50, "maxWidth": 40}),
        ),
        (
            "an unknown field",
            json!({"label": "a", "url": "/", "width": 1, "height": 1, "bogus": 1}),
        ),
        ("no size", json!({"label": "a", "url": "/"})),
        (
            "a size in em",
            json!({"label": "a", "url": "/", "width": "2em", "height": 1}),
        ),
    ] {
        let error = ask(&open, "window.create", body).await.expect_err(what);
        assert_eq!(error.code, ErrorCode::InvalidArgument, "{what}");
    }
    assert_eq!(
        calls(&open).len(),
        1,
        "only the valid definition reached the host"
    );
}

#[tokio::test]
async fn windows_and_displays_are_asked_from_the_host() {
    let fixture = Fixture::new(None, &[]).await;
    for (command, expected) in [
        ("window.all", UiCall::Windows),
        ("screen.monitors", UiCall::Monitors),
        ("screen.cursorPosition", UiCall::CursorPosition),
    ] {
        ask(&fixture, command, Value::Null).await.expect(command);
        assert_eq!(calls(&fixture).pop(), Some((1, expected)), "{command}");
    }
}
