// SPDX-License-Identifier: MIT OR Apache-2.0
use crate::common::Fixture;
use alef_core::{
    registry::{
        command::Reply,
        window::{tray::TrayCall, UiCall},
    },
    security::consent::{Consent, Decision, Right},
    AlefError, ErrorCode,
};
use bytes::Bytes;
use image::{ExtendedColorType, ImageEncoder};
use serde_json::{json, Value};
use std::{future::Future, time::Duration};
const MANIFEST: &str = include_str!("../../fixtures/app.ktav");
async fn bounded<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(5), future)
        .await
        .expect("tray operation must complete")
}
async fn fixture(decision: Decision) -> Fixture {
    let text = MANIFEST.replace("    secrets: false", "    tray: true\n    secrets: false");
    let mut consent = Consent::undecided();
    consent.set(Right::plain("tray"), decision);
    bounded(Fixture::new(Some(&text), &[]))
        .await
        .with_consent(consent)
}
fn png(width: u32, height: u32) -> Bytes {
    let mut bytes = Vec::new();
    image::codecs::png::PngEncoder::new(&mut bytes)
        .write_image(
            &vec![0; width as usize * height as usize * 4],
            width,
            height,
            ExtendedColorType::Rgba8,
        )
        .unwrap();
    bytes.into()
}
async fn binary(
    f: &Fixture,
    name: &str,
    args: Value,
    body: Option<Bytes>,
) -> Result<Value, AlefError> {
    match bounded(f.call_reply(name, args, body)).await? {
        Reply::Json(value) => Ok(value),
        other => panic!("expected JSON, got {other:?}"),
    }
}
async fn create(f: &Fixture) -> Value {
    binary(f, "tray.create", json!({}), Some(png(1, 1)))
        .await
        .unwrap()
}
fn seen(f: &Fixture) -> Vec<(u64, UiCall)> {
    f.host.calls.lock().unwrap().clone()
}

#[tokio::test]
async fn denied_and_undeclared_trays_never_call_the_host() {
    let denied = fixture(Decision::Deny).await;
    let absent = bounded(Fixture::new(None, &[])).await;
    for f in [&denied, &absent] {
        assert_eq!(
            binary(f, "tray.create", json!({}), Some(png(1, 1)))
                .await
                .unwrap_err()
                .code,
            ErrorCode::PermissionDenied
        );
        for (command, args) in [
            ("tray.setIcon", json!({"id":"s1:r1"})),
            ("tray.setTooltip", json!({"id":"s1:r1","tooltip":""})),
            ("tray.setMenu", json!({"id":"s1:r1","menu":[]})),
        ] {
            assert_eq!(
                bounded(f.call(command, args)).await.unwrap_err().code,
                ErrorCode::PermissionDenied
            );
        }
        assert_eq!(
            bounded(f.call("tray.destroy", json!({"id":"s1:r1"})))
                .await
                .unwrap_err()
                .code,
            ErrorCode::NotFound
        );
        assert!(seen(f).is_empty());
    }
}

#[tokio::test]
async fn substituted_and_native_trays_return_the_same_opaque_token_shape() {
    for decision in [Decision::Substitute, Decision::Allow] {
        let f = fixture(decision).await;
        let reply = create(&f).await;
        let id = reply["id"].as_str().unwrap();
        let token = reply["token"].as_str().unwrap();
        assert!(!token.is_empty());
        assert_eq!(
            reply,
            json!({"id":id,"owner":f.session().id().0,"token":token})
        );
        assert_eq!(id, format!("s{}:r1", f.session().id().0));
        binary(&f, "tray.setIcon", json!({"id":id}), Some(png(2, 2)))
            .await
            .unwrap();
        bounded(f.call("tray.setTooltip", json!({"id":id,"tooltip":""})))
            .await
            .unwrap();
        bounded(f.call(
            "tray.setMenu",
            json!({"id":id,"menu":[{"id":"quit","label":"Quit"}]}),
        ))
        .await
        .unwrap();
        bounded(f.call("tray.destroy", json!({"id":id})))
            .await
            .unwrap();
        assert!(f.session().resources().is_empty());
        if decision == Decision::Substitute {
            assert!(seen(&f).is_empty());
        } else {
            let owner = f.session().id();
            let token = token.to_owned();
            let calls = seen(&f);
            assert_eq!(calls.len(), 5);
            assert_eq!(
                calls[0],
                (
                    1,
                    UiCall::Tray(TrayCall::Create {
                        owner,
                        token: token.clone(),
                        icon: png(1, 1).to_vec(),
                        tooltip: String::new(),
                        menu: vec![]
                    })
                )
            );
            assert_eq!(
                calls[1],
                (
                    1,
                    UiCall::Tray(TrayCall::SetIcon {
                        owner,
                        token: token.clone(),
                        icon: png(2, 2).to_vec()
                    })
                )
            );
            assert_eq!(
                calls[2],
                (
                    1,
                    UiCall::Tray(TrayCall::SetTooltip {
                        owner,
                        token: token.clone(),
                        tooltip: String::new()
                    })
                )
            );
            assert!(
                matches!(&calls[3].1, UiCall::Tray(TrayCall::SetMenu { menu, .. }) if menu[0].id.as_deref()==Some("quit"))
            );
            assert_eq!(
                calls[4],
                (1, UiCall::Tray(TrayCall::Destroy { owner, token }))
            );
        }
    }
}

#[tokio::test]
async fn tray_handles_are_canonical_and_wrong_documents_cannot_use_them() {
    let f = fixture(Decision::Substitute).await;
    let reply = create(&f).await;
    let id = reply["id"].as_str().unwrap();
    let other = bounded(f.open_window(2)).await;
    for command in ["tray.destroy", "tray.setTooltip", "tray.setMenu"] {
        let args = match command {
            "tray.setTooltip" => json!({"id":id,"tooltip":""}),
            "tray.setMenu" => json!({"id":id,"menu":[]}),
            _ => json!({"id":id}),
        };
        assert_eq!(
            bounded(f.call_as(&other, command, args))
                .await
                .unwrap_err()
                .code,
            ErrorCode::NotFound
        );
    }
    let owner = f.session().id().0;
    for id in [
        format!("s{owner}:r01"),
        format!("s{owner}:r+1"),
        format!("s0{owner}:r1"),
        reply["token"].as_str().unwrap().into(),
    ] {
        assert_eq!(
            bounded(f.call("tray.destroy", json!({"id":id})))
                .await
                .unwrap_err()
                .code,
            ErrorCode::NotFound
        );
    }
    bounded(f.call("tray.destroy", json!({"id":reply["id"]})))
        .await
        .unwrap();
    assert_eq!(
        bounded(f.call("tray.destroy", json!({"id":reply["id"]})))
            .await
            .unwrap_err()
            .code,
        ErrorCode::NotFound
    );
}

#[tokio::test]
async fn a_tray_that_is_being_destroyed_cannot_be_changed_meanwhile() {
    let f = fixture(Decision::Allow).await;
    let created = create(&f).await;
    let id = created["id"].clone();
    let gate = std::sync::Arc::new(tokio::sync::Notify::new());
    *f.host.hold.lock().unwrap() = Some(gate.clone());
    let destroy = f.call("tray.destroy", json!({ "id": id }));
    let change = async {
        // The destroy has reached the host and holds the tray; the change has to wait behind it.
        while !seen(&f)
            .iter()
            .any(|(_, call)| matches!(call, UiCall::Tray(TrayCall::Destroy { .. })))
        {
            tokio::time::sleep(Duration::from_millis(5)).await;
        }
        let attempt = f.call("tray.setTooltip", json!({ "id": id, "tooltip": "late" }));
        let release = async {
            tokio::time::sleep(Duration::from_millis(100)).await;
            gate.notify_one();
        };
        tokio::join!(attempt, release).0
    };
    let (destroyed, changed) = bounded(async { tokio::join!(destroy, change) }).await;
    destroyed.unwrap();
    assert_eq!(changed.unwrap_err().code, ErrorCode::NotFound);
    assert!(
        !seen(&f)
            .iter()
            .any(|(_, call)| matches!(call, UiCall::Tray(TrayCall::SetTooltip { .. }))),
        "nothing was sent for a tray that is gone"
    );
}

#[tokio::test]
async fn eight_trays_fill_each_document_and_destroy_recovers_the_quota() {
    for decision in [Decision::Allow, Decision::Substitute] {
        let f = fixture(decision).await;
        let mut replies = Vec::new();
        for _ in 0..8 {
            replies.push(create(&f).await);
        }
        assert_eq!(
            binary(&f, "tray.create", json!({}), Some(png(1, 1)))
                .await
                .unwrap_err()
                .code,
            ErrorCode::Busy
        );
        assert_eq!(f.session().resources().len(), 8);
        bounded(f.call("tray.destroy", json!({"id":replies[0]["id"]})))
            .await
            .unwrap();
        let fresh = create(&f).await;
        assert!(replies.iter().all(|old| old["token"] != fresh["token"]));
        assert_eq!(f.session().resources().len(), 8);
    }
}

#[tokio::test]
async fn reload_releases_native_trays_and_invalidates_old_handles() {
    let mut f = fixture(Decision::Allow).await;
    let owner = f.session().id();
    let old = create(&f).await;
    bounded(f.reload_document()).await;
    assert_eq!(
        seen(&f).last().unwrap(),
        &(
            1,
            UiCall::Tray(TrayCall::Destroy {
                owner,
                token: old["token"].as_str().unwrap().into()
            })
        )
    );
    assert_eq!(
        bounded(f.call("tray.destroy", json!({"id":old["id"]})))
            .await
            .unwrap_err()
            .code,
        ErrorCode::NotFound
    );
    for _ in 0..8 {
        let new = create(&f).await;
        assert_ne!(new["token"], old["token"]);
    }
    bounded(f.session().close()).await;
    assert_eq!(seen(&f).len(), 18);
}

#[tokio::test]
async fn failed_native_creation_and_failed_destruction_preserve_recoverable_quota() {
    let f = fixture(Decision::Allow).await;
    f.host
        .replies
        .lock()
        .unwrap()
        .push_back(Err(AlefError::new(ErrorCode::NotAvailable, "no host")));
    assert_eq!(
        binary(&f, "tray.create", json!({}), Some(png(1, 1)))
            .await
            .unwrap_err()
            .code,
        ErrorCode::NotAvailable
    );
    assert!(f.session().resources().is_empty());
    let reply = create(&f).await;
    f.host
        .replies
        .lock()
        .unwrap()
        .push_back(Err(AlefError::new(ErrorCode::NotAvailable, "no host")));
    assert_eq!(
        bounded(f.call("tray.destroy", json!({"id":reply["id"]})))
            .await
            .unwrap_err()
            .code,
        ErrorCode::NotAvailable
    );
    assert_eq!(f.session().resources().len(), 1);
    bounded(f.call("tray.destroy", json!({"id":reply["id"]})))
        .await
        .unwrap();
    for _ in 0..8 {
        create(&f).await;
    }
}

#[tokio::test]
async fn malformed_pngs_and_out_of_bounds_images_are_rejected_even_when_substituted() {
    for decision in [Decision::Substitute, Decision::Allow] {
        let f = fixture(decision).await;
        for body in [
            None,
            Some(Bytes::new()),
            Some(Bytes::from_static(b"not PNG")),
            Some(Bytes::from(vec![0; 256 * 1024 + 1])),
            Some(png(257, 1)),
            Some(png(1, 257)),
            Some(png(256, 256).slice(..30)),
        ] {
            assert_eq!(
                binary(&f, "tray.create", json!({}), body)
                    .await
                    .unwrap_err()
                    .code,
                ErrorCode::InvalidArgument
            );
        }
        assert!(seen(&f).is_empty());
        let reply = binary(&f, "tray.create", json!({}), Some(png(256, 256)))
            .await
            .unwrap();
        assert_eq!(
            binary(
                &f,
                "tray.setIcon",
                json!({"id":reply["id"]}),
                Some(png(257, 1))
            )
            .await
            .unwrap_err()
            .code,
            ErrorCode::InvalidArgument
        );
    }
}

use alef_core::session::Resource;
use std::pin::Pin;
struct OtherResource;
impl Resource for OtherResource {
    fn close(self: Box<Self>) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        Box::pin(async {})
    }
}

#[tokio::test]
async fn failed_resource_insertion_rolls_back_native_creation_without_consuming_quota() {
    let f = fixture(Decision::Allow).await;
    let session = f.session();
    let mut ids = Vec::new();
    for _ in 0..session.resources().limit() {
        ids.push(session.resources().insert(Box::new(OtherResource)).unwrap());
    }
    assert_eq!(
        binary(&f, "tray.create", json!({}), Some(png(1, 1)))
            .await
            .unwrap_err()
            .code,
        ErrorCode::Busy
    );
    let calls = seen(&f);
    assert_eq!(calls.len(), 2);
    match (&calls[0].1, &calls[1].1) {
        (
            UiCall::Tray(TrayCall::Create {
                owner: a, token: x, ..
            }),
            UiCall::Tray(TrayCall::Destroy { owner: b, token: y }),
        ) => {
            assert_eq!(a, b);
            assert_eq!(x, y);
        }
        _ => panic!("creation must be rolled back"),
    }
    for id in ids {
        session.resources().remove(id).unwrap();
    }
    for _ in 0..8 {
        create(&f).await;
    }
}

#[tokio::test]
async fn concurrent_substituted_creations_are_serialized_at_the_eight_tray_limit() {
    let f = std::sync::Arc::new(fixture(Decision::Substitute).await);
    let mut jobs = Vec::new();
    for _ in 0..16 {
        let f = f.clone();
        jobs.push(tokio::spawn(async move {
            binary(&f, "tray.create", json!({}), Some(png(1, 1))).await
        }));
    }
    let mut successes = 0;
    for job in jobs {
        match bounded(job).await.unwrap() {
            Ok(_) => successes += 1,
            Err(error) => assert_eq!(error.code, ErrorCode::Busy),
        }
    }
    assert_eq!(successes, 8);
    assert_eq!(f.session().resources().len(), 8);
    assert!(seen(&f).is_empty());
    bounded(f.session().close()).await;
}

#[tokio::test]
async fn invalid_native_acknowledgements_roll_back_and_leave_no_resource() {
    let f = fixture(Decision::Allow).await;
    f.host
        .replies
        .lock()
        .unwrap()
        .push_back(Ok(json!("unexpected token")));
    assert_eq!(
        binary(&f, "tray.create", json!({}), Some(png(1, 1)))
            .await
            .unwrap_err()
            .code,
        ErrorCode::Internal
    );
    assert!(f.session().resources().is_empty());
    assert!(matches!(
        seen(&f).last().unwrap().1,
        UiCall::Tray(TrayCall::Destroy { .. })
    ));
    for _ in 0..8 {
        create(&f).await;
    }
}

#[tokio::test]
async fn creation_after_session_teardown_rolls_back_and_does_not_insert_a_resource() {
    let f = fixture(Decision::Allow).await;
    bounded(f.session().resources().close_all()).await;
    assert_eq!(
        binary(&f, "tray.create", json!({}), Some(png(1, 1)))
            .await
            .unwrap_err()
            .code,
        ErrorCode::Closed
    );
    assert!(f.session().resources().is_empty());
    assert_eq!(seen(&f).len(), 2);
    assert!(matches!(
        seen(&f)[1].1,
        UiCall::Tray(TrayCall::Destroy { .. })
    ));
}

#[tokio::test]
async fn failed_native_setters_leave_the_resource_available_for_cleanup() {
    let f = fixture(Decision::Allow).await;
    let reply = create(&f).await;
    for command in ["tray.setIcon", "tray.setTooltip", "tray.setMenu"] {
        f.host
            .replies
            .lock()
            .unwrap()
            .push_back(Err(AlefError::new(ErrorCode::NotAvailable, "no host")));
        let args = match command {
            "tray.setTooltip" => json!({"id":reply["id"],"tooltip":"Tooltip"}),
            "tray.setMenu" => json!({"id":reply["id"],"menu":[]}),
            _ => json!({"id":reply["id"]}),
        };
        let body = (command == "tray.setIcon").then(|| png(1, 1));
        assert_eq!(
            binary(&f, command, args, body).await.unwrap_err().code,
            ErrorCode::NotAvailable
        );
        assert_eq!(f.session().resources().len(), 1);
    }
    bounded(f.call("tray.destroy", json!({"id":reply["id"]})))
        .await
        .unwrap();
    assert!(f.session().resources().is_empty());
}

#[tokio::test]
async fn malformed_metadata_and_unexpected_bodies_are_rejected_without_host_calls() {
    let f = fixture(Decision::Substitute).await;
    for args in [
        Value::Null,
        json!({"icon":"path"}),
        json!({"tooltip":1}),
        json!({"tooltip":"é".repeat(65)}),
        json!({"tooltip":"\n"}),
        json!({"menu":[{"id":"x","label":"X","accelerator":"Ctrl+X"}]}),
        json!({"menu":[{"kind":"submenu","id":"p","label":"P","items":[{"id":"x","label":"X","accelerator":"Ctrl+X"}]}]}),
        json!({"menu":[{"id":"x","label":"X","unknown":true}]}),
        json!({"menu":[{"id":"x"}]}),
    ] {
        assert_eq!(
            binary(&f, "tray.create", args, Some(png(1, 1)))
                .await
                .unwrap_err()
                .code,
            ErrorCode::InvalidArgument
        );
    }
    let reply = create(&f).await;
    for (command, args) in [
        ("tray.setTooltip", json!({"id":reply["id"],"tooltip":""})),
        ("tray.setMenu", json!({"id":reply["id"],"menu":[]})),
        ("tray.destroy", json!({"id":reply["id"]})),
    ] {
        assert_eq!(
            binary(&f, command, args, Some(png(1, 1)))
                .await
                .unwrap_err()
                .code,
            ErrorCode::InvalidArgument
        );
    }
    assert!(seen(&f).is_empty());
}
