// SPDX-License-Identifier: MIT OR Apache-2.0
//! `notification` through the registry: what is checked, what reaches the desktop, and the icon.

use std::{fs, path::Path};

use crate::common::Fixture;
use alef_core::ErrorCode;
use alef_modules::Notification;
use serde_json::{json, Value};

const MANIFEST: &str = include_str!("../fixtures/app.ktav");

/// Files readable under `$TEMP`, the way an application that shows icons of its own would ask.
fn reading_temp() -> String {
    let text = MANIFEST.replace('\r', "").replace(
        "        read: []\n        write: []",
        "        read: [ $TEMP/** ]\n        write: []",
    );
    assert!(text.contains("$TEMP/**"));
    text
}

fn workdir(fixture: &Fixture) -> tempfile::TempDir {
    fs::create_dir_all(&fixture.context.paths.temp).unwrap();
    tempfile::Builder::new()
        .prefix("notification-")
        .tempdir_in(&fixture.context.paths.temp)
        .unwrap()
}

fn canonical(path: &Path) -> std::path::PathBuf {
    let resolved = fs::canonicalize(path).unwrap();
    Path::new(resolved.to_string_lossy().trim_start_matches(r"\\?\")).to_owned()
}

fn shown(fixture: &Fixture) -> Vec<Notification> {
    fixture.notifications.shown()
}

#[tokio::test]
async fn a_notification_reaches_the_desktop_with_its_text() {
    let fixture = Fixture::new(None, &[]).await;
    let reply = fixture
        .call(
            "notification.show",
            json!({ "title": "Done", "body": "All saved\nin two places" }),
        )
        .await
        .expect("show");
    assert_eq!(reply, Value::Null);
    fixture
        .call("notification.show", json!({ "title": "Only a title" }))
        .await
        .expect("a body is optional");
    assert_eq!(
        shown(&fixture),
        [
            Notification {
                title: "Done".to_owned(),
                body: "All saved\nin two places".to_owned(),
                icon: None
            },
            Notification {
                title: "Only a title".to_owned(),
                body: String::new(),
                icon: None
            },
        ]
    );
}

#[tokio::test]
async fn text_that_is_not_fit_to_show_is_refused_before_the_desktop_hears_of_it() {
    let fixture = Fixture::new(None, &[]).await;
    let cases = [
        json!({}),
        json!({ "title": "" }),
        json!({ "title": "   " }),
        json!({ "title": "t".repeat(129) }),
        json!({ "title": "two\nlines" }),
        json!({ "title": "bell\u{7}" }),
        json!({ "title": "t", "body": "b".repeat(1025) }),
        json!({ "title": "t", "body": "nul\u{0}" }),
        json!({ "title": "t", "body": 5 }),
        json!({ "title": "t", "bogus": 1 }),
        json!({ "title": 7 }),
        json!(["title"]),
        Value::Null,
    ];
    for body in cases {
        let error = fixture
            .call("notification.show", body.clone())
            .await
            .expect_err(&body.to_string());
        assert_eq!(error.code, ErrorCode::InvalidArgument, "{body}");
    }
    assert!(shown(&fixture).is_empty());
    fixture
        .call(
            "notification.show",
            json!({ "title": "t".repeat(128), "body": format!("a\tb\n{}", "b".repeat(1018)) }),
        )
        .await
        .expect("the limits themselves, a tab and a line break are fine");
    assert_eq!(shown(&fixture).len(), 1);
}

#[tokio::test]
async fn an_icon_the_document_may_read_is_passed_as_the_checked_path() {
    let fixture = Fixture::new(Some(&reading_temp()), &[]).await;
    let directory = workdir(&fixture);
    let icon = directory.path().join("icon.png");
    fs::write(&icon, "not really a picture").unwrap();
    let detour = directory.path().join(".").join("icon.png");
    fixture
        .call(
            "notification.show",
            json!({ "title": "With an icon", "icon": detour.to_string_lossy() }),
        )
        .await
        .expect("show");
    assert_eq!(
        shown(&fixture),
        [Notification {
            title: "With an icon".to_owned(),
            body: String::new(),
            icon: Some(canonical(&icon)),
        }],
        "the desktop gets the path that was checked, not the one the document wrote"
    );
}

#[tokio::test]
async fn an_icon_outside_the_read_scope_is_denied_and_a_missing_or_wrong_one_is_named() {
    let fixture = Fixture::new(Some(&reading_temp()), &[]).await;
    let directory = workdir(&fixture);
    let outside = tempfile::tempdir().unwrap();
    let stray = outside.path().join("stray.png");
    fs::write(&stray, "x").unwrap();
    let folder = directory.path().join("folder");
    fs::create_dir(&folder).unwrap();
    for (what, icon, code) in [
        (
            "outside the scope",
            stray.to_string_lossy().into_owned(),
            ErrorCode::PermissionDenied,
        ),
        (
            "relative",
            "icon.png".to_owned(),
            ErrorCode::PermissionDenied,
        ),
        (
            "missing",
            directory
                .path()
                .join("never-was.png")
                .to_string_lossy()
                .into_owned(),
            ErrorCode::NotFound,
        ),
        (
            "a folder",
            folder.to_string_lossy().into_owned(),
            ErrorCode::InvalidArgument,
        ),
    ] {
        let error = fixture
            .call("notification.show", json!({ "title": "t", "icon": icon }))
            .await
            .expect_err(what);
        assert_eq!(error.code, code, "{what}");
    }
    assert!(shown(&fixture).is_empty());
}

#[tokio::test]
async fn without_read_access_no_icon_may_be_named_but_a_plain_notification_needs_none() {
    let fixture = Fixture::new(None, &[]).await;
    let directory = tempfile::tempdir().unwrap();
    let icon = directory.path().join("icon.png");
    fs::write(&icon, "x").unwrap();
    let error = fixture
        .call(
            "notification.show",
            json!({ "title": "t", "icon": icon.to_string_lossy() }),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::PermissionDenied);
    fixture
        .call("notification.show", json!({ "title": "t" }))
        .await
        .expect("a notification without an icon needs no right");
    assert_eq!(shown(&fixture).len(), 1);
}
