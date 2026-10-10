// SPDX-License-Identifier: MIT OR Apache-2.0
use crate::common::Fixture;
use alef_core::{
    registry::{
        context::CallContext,
        window::{
            menu::{MenuCall, MenuItem},
            UiCall,
        },
    },
    session::Resource,
    ErrorCode,
};
use serde_json::{json, Value};
use std::{future::Future, pin::Pin, time::Duration};

const COMMANDS: [&str; 3] = [
    "menu.setApplicationMenu",
    "menu.setWindowMenu",
    "menu.popup",
];
fn items() -> Value {
    json!([{ "id":"go", "label":"Go", "accelerator":"CommandOrControl+K" }])
}
fn seen(f: &Fixture) -> Vec<(u64, UiCall)> {
    f.host.calls.lock().unwrap().clone()
}
fn queue(f: &Fixture, value: Value) {
    f.host.replies.lock().unwrap().push_back(Ok(value));
}
async fn bounded<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(5), future)
        .await
        .expect("menu operation completes")
}

#[tokio::test]
async fn arguments_and_bodies_are_rejected_before_host_calls() {
    let f = Fixture::new(None, &[]).await;
    for command in COMMANDS {
        for args in [
            Value::Null,
            json!({}),
            json!({"items":null}),
            json!({"items":[],"owner":7}),
            json!({"items":[{"id":"a","label":"A","unknown":1}]}),
            json!({"items":[{"id":"a","label":"A"},{"id":"a","label":"B"}]}),
            json!({"items":[{"kind":"submenu","id":"s","label":"S","items":[{"id":"a","label":"A","accelerator":"Bogus+K"}]}]}),
            json!({"items":[{"kind":"separator","id":"a"}]}),
        ] {
            assert_eq!(
                f.call(command, args).await.unwrap_err().code,
                ErrorCode::InvalidArgument
            );
        }
        assert_eq!(
            f.call_reply(
                command,
                json!({"items":items()}),
                Some(bytes::Bytes::from_static(b"x"))
            )
            .await
            .unwrap_err()
            .code,
            ErrorCode::InvalidArgument
        );
    }
    for args in [
        json!({"items":[],"label":"other"}),
        json!({"items":[],"x":1000001,"y":0}),
        json!({"items":[],"x":0,"y":"bad"}),
        json!({"items":[],"x":1}),
        json!({"items":[],"y":1}),
    ] {
        assert_eq!(
            f.call("menu.popup", args).await.unwrap_err().code,
            ErrorCode::InvalidArgument
        );
    }
    assert_eq!(
        f.call(
            "menu.setApplicationMenu",
            json!({"items":[],"label":"other"})
        )
        .await
        .unwrap_err()
        .code,
        ErrorCode::InvalidArgument
    );
    assert!(seen(&f).is_empty());
    assert!(f.session().resources().is_empty());
}

#[tokio::test]
async fn no_permission_and_owner_caller_target_propagation() {
    let f = Fixture::new(None, &[]).await;
    let session = f.open_window(42).await;
    let owner = session.id();
    let decoded: Vec<MenuItem> = serde_json::from_value(items()).unwrap();
    assert_eq!(
        f.call_as(&session, COMMANDS[0], json!({"items":items()}))
            .await
            .unwrap(),
        Value::Null
    );
    assert_eq!(
        f.call_as(
            &session,
            COMMANDS[1],
            json!({"label":"target","items":items()})
        )
        .await
        .unwrap(),
        Value::Null
    );
    queue(&f, json!("go"));
    assert_eq!(
        f.call_as(
            &session,
            COMMANDS[2],
            json!({"items":items(),"x":-1.5,"y":2})
        )
        .await
        .unwrap(),
        json!("go")
    );
    assert_eq!(
        seen(&f),
        vec![
            (
                42,
                UiCall::Menu(MenuCall::SetApplication {
                    owner,
                    items: decoded.clone()
                })
            ),
            (
                42,
                UiCall::Menu(MenuCall::SetWindow {
                    owner,
                    label: Some("target".into()),
                    items: decoded.clone()
                })
            ),
            (
                42,
                UiCall::Menu(MenuCall::Popup {
                    owner,
                    label: None,
                    items: decoded,
                    x: Some(-1.5),
                    y: Some(2.0)
                })
            ),
        ]
    );
    assert_eq!(session.resources().len(), 1);
}

#[tokio::test]
async fn malformed_set_replies_are_internal_and_remain_owned() {
    let f = Fixture::new(None, &[]).await;
    for command in &COMMANDS[..2] {
        for reply in [json!("go"), json!(false), json!(7), json!({}), json!([])] {
            queue(&f, reply);
            assert_eq!(
                f.call(command, json!({"items":items()}))
                    .await
                    .unwrap_err()
                    .code,
                ErrorCode::Internal
            );
            assert_eq!(f.session().resources().len(), 1);
        }
    }
    f.session().close().await;
    assert!(matches!(
        seen(&f).last(),
        Some((1, UiCall::Menu(MenuCall::Release { .. })))
    ));
}

#[tokio::test]
async fn popup_only_returns_enabled_actionable_custom_ids() {
    let f = Fixture::new(None, &[]).await;
    let tree = json!([
        {"id":"go","label":"Go"},
        {"kind":"check","id":"check","label":"Check"},
        {"id":"disabled","label":"Disabled","enabled":false},
        {"kind":"submenu","id":"sub","label":"Sub","items":[{"id":"nested","label":"Nested"}]},
        {"kind":"submenu","id":"blocked","label":"Blocked","enabled":false,"items":[{"id":"child","label":"Child"}]},
        {"role":"copy"},{"kind":"separator"}
    ]);
    for reply in [Value::Null, json!("go"), json!("check"), json!("nested")] {
        queue(&f, reply.clone());
        assert_eq!(
            f.call(COMMANDS[2], json!({"items":tree})).await.unwrap(),
            reply
        );
    }
    for reply in [
        json!("disabled"),
        json!("child"),
        json!("sub"),
        json!("blocked"),
        json!("copy"),
        json!("separator"),
        json!("missing"),
        json!(false),
        json!(3),
        json!({}),
        json!([]),
    ] {
        queue(&f, reply);
        assert_eq!(
            f.call(COMMANDS[2], json!({"items":tree}))
                .await
                .unwrap_err()
                .code,
            ErrorCode::Internal
        );
    }
    assert_eq!(f.session().resources().len(), 1);
}

#[tokio::test]
async fn replacements_reuse_one_resource_and_teardown_releases_original_owner() {
    let mut f = Fixture::new(None, &[]).await;
    let old = f.session();
    for _ in 0..20 {
        for command in COMMANDS {
            f.call(command, json!({"items":[]})).await.unwrap();
        }
    }
    assert_eq!(old.resources().len(), 1);
    f.reload_document().await;
    assert_eq!(
        seen(&f).last(),
        Some(&(1, UiCall::Menu(MenuCall::Release { owner: old.id() })))
    );
    f.call(COMMANDS[1], json!({"items":[]})).await.unwrap();
    f.session().close().await;
    assert_eq!(
        seen(&f).last(),
        Some(&(
            1,
            UiCall::Menu(MenuCall::Release {
                owner: f.session().id()
            })
        ))
    );
    assert_ne!(old.id(), f.session().id());
}

struct Other;
impl Resource for Other {
    fn close(self: Box<Self>) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        Box::pin(async {})
    }
}
#[tokio::test]
async fn insertion_failure_rolls_back_original_owner() {
    let f = Fixture::new(None, &[]).await;
    let session = f.session();
    for _ in 0..session.resources().limit() {
        session.resources().insert(Box::new(Other)).unwrap();
    }
    assert_eq!(
        f.call(COMMANDS[0], json!({"items":[]}))
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
                UiCall::Menu(MenuCall::SetApplication {
                    owner: session.id(),
                    items: vec![]
                })
            ),
            (
                1,
                UiCall::Menu(MenuCall::Release {
                    owner: session.id()
                })
            )
        ]
    );
}

#[tokio::test]
async fn cancelled_transaction_rolls_back_after_session_close() {
    let f = Fixture::new(None, &[]).await;
    let session = f.session();
    let ctx = CallContext::new(session.clone(), f.permissions());
    {
        let dispatch = f.registry.dispatch(COMMANDS[0], ctx, json!({"items":[]}));
        tokio::pin!(dispatch);
        let mut task = std::task::Context::from_waker(std::task::Waker::noop());
        assert!(dispatch.as_mut().poll(&mut task).is_pending());
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
                UiCall::Menu(MenuCall::SetApplication {
                    owner: session.id(),
                    items: vec![]
                })
            ),
            (
                1,
                UiCall::Menu(MenuCall::Release {
                    owner: session.id()
                })
            )
        ]
    );
    assert!(session.resources().is_empty());
}
