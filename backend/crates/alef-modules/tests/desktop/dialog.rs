// SPDX-License-Identifier: MIT OR Apache-2.0
//! `dialog` through the registry: options, what reaches the host, and the grants a choice makes.

use std::{fs, path::Path};

use crate::common::Fixture;
use alef_core::{
    registry::{
        dialog::{
            ConfirmOptions, DialogCall, FileFilter, MessageKind, MessageOptions, OpenOptions,
            SaveOptions,
        },
        window::UiCall,
    },
    security::permissions::Permission,
    AlefError, ErrorCode,
};
use serde_json::{json, Value};

fn text(path: &Path) -> String {
    path.to_string_lossy().into_owned()
}

fn dialogs(fixture: &Fixture) -> Vec<(u64, DialogCall)> {
    fixture
        .host
        .calls
        .lock()
        .unwrap()
        .iter()
        .map(|(caller, call)| match call {
            UiCall::Dialog(dialog) => (*caller, dialog.clone()),
            other => panic!("not a dialog: {other:?}"),
        })
        .collect()
}

fn answer(fixture: &Fixture, reply: Result<Value, AlefError>) {
    fixture.host.replies.lock().unwrap().push_back(reply);
}

fn may(fixture: &Fixture, permission: Permission, path: &Path) -> bool {
    fixture
        .permissions()
        .check(permission, Some(&text(path)), &fixture.session().grants())
        .is_ok()
}

fn code(error: &AlefError) -> ErrorCode {
    error.code
}

#[tokio::test]
async fn open_reaches_the_host_with_the_options_and_the_window_of_the_caller() {
    let fixture = Fixture::new(None, &[]).await;
    let start = std::env::temp_dir();
    answer(&fixture, Ok(json!([])));
    let reply = fixture
        .call(
            "dialog.open",
            json!({
                "title": "Pick",
                "multiple": true,
                "defaultPath": text(&start),
                "filters": [{ "name": "Images", "extensions": ["png", "jpg"] }]
            }),
        )
        .await
        .expect("open");
    assert_eq!(reply, json!([]), "a cancelled dialog answers with no paths");
    assert_eq!(
        dialogs(&fixture),
        vec![(
            fixture.session().window(),
            DialogCall::Open(OpenOptions {
                title: Some("Pick".to_owned()),
                filters: Some(vec![FileFilter {
                    name: "Images".to_owned(),
                    extensions: vec!["png".to_owned(), "jpg".to_owned()],
                }]),
                multiple: Some(true),
                directory: None,
                default_path: Some(text(&start)),
            })
        )]
    );
    answer(&fixture, Ok(json!([])));
    fixture
        .call("dialog.open", Value::Null)
        .await
        .expect("no options");
    assert_eq!(
        dialogs(&fixture)[1].1,
        DialogCall::Open(OpenOptions::default())
    );
}

#[tokio::test]
async fn a_chosen_file_is_readable_for_the_session_and_nothing_else_is() {
    let directory = tempfile::tempdir().unwrap();
    let chosen = directory.path().join("chosen.txt");
    let sibling = directory.path().join("sibling.txt");
    fs::write(&chosen, "a").unwrap();
    fs::write(&sibling, "b").unwrap();
    let fixture = Fixture::new(None, &[]).await;
    assert!(
        !may(&fixture, Permission::FsRead, &chosen),
        "before the dialog the file is out of reach"
    );
    answer(&fixture, Ok(json!([text(&chosen)])));
    let reply = fixture.call("dialog.open", json!({})).await.expect("open");
    assert_eq!(reply, json!([text(&chosen)]));
    assert!(may(&fixture, Permission::FsRead, &chosen));
    assert!(!may(&fixture, Permission::FsRead, &sibling));
    assert!(
        !may(&fixture, Permission::FsWrite, &chosen),
        "reading a choice does not allow writing it"
    );
}

#[tokio::test]
async fn a_chosen_folder_is_readable_with_everything_below_it() {
    let directory = tempfile::tempdir().unwrap();
    let folder = directory.path().join("picked");
    let inside = folder.join("deep").join("note.txt");
    let beside = directory.path().join("beside.txt");
    fs::create_dir_all(inside.parent().unwrap()).unwrap();
    fs::write(&inside, "a").unwrap();
    fs::write(&beside, "b").unwrap();
    let fixture = Fixture::new(None, &[]).await;
    answer(&fixture, Ok(json!([text(&folder)])));
    fixture
        .call("dialog.open", json!({ "directory": true }))
        .await
        .expect("open a folder");
    assert!(may(&fixture, Permission::FsRead, &folder));
    assert!(may(&fixture, Permission::FsRead, &inside));
    assert!(!may(&fixture, Permission::FsRead, &beside));
    assert!(!may(&fixture, Permission::FsWrite, &inside));
}

#[tokio::test]
async fn several_chosen_files_are_all_granted() {
    let directory = tempfile::tempdir().unwrap();
    let files: Vec<_> = ["one.txt", "two.txt", "three.txt"]
        .iter()
        .map(|name| {
            let path = directory.path().join(name);
            fs::write(&path, "x").unwrap();
            path
        })
        .collect();
    let fixture = Fixture::new(None, &[]).await;
    answer(&fixture, Ok(json!([text(&files[0]), text(&files[1])])));
    fixture
        .call("dialog.open", json!({ "multiple": true }))
        .await
        .expect("open");
    assert!(may(&fixture, Permission::FsRead, &files[0]));
    assert!(may(&fixture, Permission::FsRead, &files[1]));
    assert!(!may(&fixture, Permission::FsRead, &files[2]));
}

#[tokio::test]
async fn a_cancelled_dialog_grants_nothing() {
    let fixture = Fixture::new(None, &[]).await;
    answer(&fixture, Ok(json!([])));
    fixture.call("dialog.open", json!({})).await.expect("open");
    answer(&fixture, Ok(Value::Null));
    let saved = fixture.call("dialog.save", json!({})).await.expect("save");
    assert_eq!(saved, Value::Null);
    assert!(fixture.session().grants().is_empty());
}

#[tokio::test]
async fn a_chosen_save_path_is_writable_as_that_one_file_and_not_readable() {
    let directory = tempfile::tempdir().unwrap();
    let target = directory.path().join("report.txt");
    let other = directory.path().join("other.txt");
    let fixture = Fixture::new(None, &[]).await;
    answer(&fixture, Ok(json!(text(&target))));
    let reply = fixture
        .call(
            "dialog.save",
            json!({ "title": "Save", "defaultPath": text(directory.path()) }),
        )
        .await
        .expect("save");
    assert_eq!(reply, json!(text(&target)));
    assert!(
        may(&fixture, Permission::FsWrite, &target),
        "the file does not exist yet and is still granted"
    );
    assert!(!may(&fixture, Permission::FsWrite, &other));
    assert!(!may(&fixture, Permission::FsWrite, directory.path()));
    assert!(!may(&fixture, Permission::FsRead, &target));
    assert_eq!(
        dialogs(&fixture)[0].1,
        DialogCall::Save(SaveOptions {
            title: Some("Save".to_owned()),
            filters: None,
            default_path: Some(text(directory.path())),
        })
    );
}

#[tokio::test]
async fn grants_end_with_the_session_of_the_document() {
    let directory = tempfile::tempdir().unwrap();
    let chosen = directory.path().join("chosen.txt");
    fs::write(&chosen, "a").unwrap();
    let mut fixture = Fixture::new(None, &[]).await;
    answer(&fixture, Ok(json!([text(&chosen)])));
    fixture.call("dialog.open", json!({})).await.expect("open");
    assert!(may(&fixture, Permission::FsRead, &chosen));
    fixture.reload_document().await;
    assert!(
        !may(&fixture, Permission::FsRead, &chosen),
        "the document that loaded again has not been given the file"
    );
}

#[tokio::test]
async fn message_and_confirm_carry_their_options_and_answer_plainly() {
    let fixture = Fixture::new(None, &[]).await;
    let nothing = fixture
        .call(
            "dialog.message",
            json!({ "title": "Done", "message": "All saved", "kind": "warning" }),
        )
        .await
        .expect("message");
    assert_eq!(nothing, Value::Null);
    answer(&fixture, Ok(json!(true)));
    let confirmed = fixture
        .call(
            "dialog.confirm",
            json!({ "message": "Delete?", "okLabel": "Delete", "cancelLabel": "Keep" }),
        )
        .await
        .expect("confirm");
    assert_eq!(confirmed, json!(true));
    answer(&fixture, Ok(json!(false)));
    let declined = fixture
        .call("dialog.confirm", json!({ "message": "Again?" }))
        .await
        .expect("confirm");
    assert_eq!(declined, json!(false));
    let calls = dialogs(&fixture);
    assert_eq!(
        calls[0].1,
        DialogCall::Message(MessageOptions {
            title: Some("Done".to_owned()),
            message: "All saved".to_owned(),
            kind: Some(MessageKind::Warning),
        })
    );
    assert_eq!(
        calls[1].1,
        DialogCall::Confirm(ConfirmOptions {
            title: None,
            message: "Delete?".to_owned(),
            ok_label: Some("Delete".to_owned()),
            cancel_label: Some("Keep".to_owned()),
        })
    );
}

#[tokio::test]
async fn broken_options_are_refused_before_the_host_is_asked() {
    let fixture = Fixture::new(None, &[]).await;
    let cases = [
        ("dialog.open", json!({ "bogus": 1 })),
        ("dialog.open", json!({ "multiple": "yes" })),
        ("dialog.open", json!({ "defaultPath": "relative/path" })),
        (
            "dialog.open",
            json!({ "filters": [{ "name": "x", "extensions": [".png"] }] }),
        ),
        (
            "dialog.open",
            json!({ "directory": true, "filters": [{ "name": "x", "extensions": ["png"] }] }),
        ),
        ("dialog.open", json!(["not", "an", "object"])),
        ("dialog.save", json!({ "title": "a\nb" })),
        (
            "dialog.save",
            json!({ "filters": [{ "name": "", "extensions": ["png"] }] }),
        ),
        ("dialog.message", Value::Null),
        ("dialog.message", json!({})),
        ("dialog.message", json!({ "message": "m", "kind": "fatal" })),
        ("dialog.message", json!({ "message": "m".repeat(9000) })),
        (
            "dialog.confirm",
            json!({ "message": "m", "okLabel": "Same", "cancelLabel": "Same" }),
        ),
        ("dialog.confirm", json!({ "okLabel": "OK" })),
    ];
    for (command, body) in cases {
        let error = fixture
            .call(command, body.clone())
            .await
            .expect_err(&format!("{command} {body}"));
        assert_eq!(code(&error), ErrorCode::InvalidArgument, "{command} {body}");
    }
    assert!(dialogs(&fixture).is_empty(), "the host was never asked");
}

#[tokio::test]
async fn an_error_or_an_odd_answer_of_the_host_grants_nothing() {
    let directory = tempfile::tempdir().unwrap();
    let chosen = directory.path().join("chosen.txt");
    fs::write(&chosen, "a").unwrap();
    let fixture = Fixture::new(None, &[]).await;
    answer(
        &fixture,
        Err(AlefError::new(ErrorCode::NotAvailable, "no dialogs here")),
    );
    let refused = fixture.call("dialog.open", json!({})).await.unwrap_err();
    assert_eq!(code(&refused), ErrorCode::NotAvailable);
    for (command, reply) in [
        ("dialog.open", json!(text(&chosen))),
        ("dialog.open", json!([1, 2])),
        ("dialog.save", json!([text(&chosen)])),
        ("dialog.confirm", json!("yes")),
    ] {
        answer(&fixture, Ok(reply.clone()));
        let body = if command == "dialog.confirm" {
            json!({ "message": "m" })
        } else {
            json!({})
        };
        let error = fixture.call(command, body).await.unwrap_err();
        assert_eq!(code(&error), ErrorCode::Internal, "{command} {reply}");
    }
    answer(&fixture, Ok(json!(["relative.txt"])));
    let relative = fixture.call("dialog.open", json!({})).await.unwrap_err();
    assert_eq!(code(&relative), ErrorCode::InvalidArgument);
    assert!(fixture.session().grants().is_empty());
}
