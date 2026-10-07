// SPDX-License-Identifier: MIT OR Apache-2.0
//! The values an application keeps (`store`), through the registry.
use std::path::Path;

use alef_core::ErrorCode;
use serde_json::{json, Value};

use crate::common::Fixture;

async fn app(root: &Path) -> Fixture {
    Fixture::new_in(root, None, &[]).await
}

async fn set(app: &Fixture, key: &str, value: Value) {
    app.call("store.set", json!({ "key": key, "value": value }))
        .await
        .unwrap();
}

async fn get(app: &Fixture, key: &str) -> Value {
    app.call("store.get", json!({ "key": key })).await.unwrap()
}

async fn keys(app: &Fixture, args: Value) -> Vec<String> {
    let reply = app.call("store.keys", args).await.unwrap();
    serde_json::from_value(reply).unwrap()
}

#[tokio::test]
async fn a_value_comes_back_as_it_went_and_nothing_stored_is_not_a_null() {
    let root = tempfile::tempdir().unwrap();
    let app = app(root.path()).await;
    let values = [
        json!("text — with ünicode and \u{1F600}"),
        json!(42),
        json!(-1.5e300),
        json!(true),
        json!(null),
        json!({ "a": [1, 2, { "b": null }], "c": "d" }),
        json!([]),
    ];
    for (index, value) in values.iter().enumerate() {
        let key = format!("k{index}");
        set(&app, &key, value.clone()).await;
        assert_eq!(get(&app, &key).await, json!({ "value": value }), "{key}");
    }
    assert_eq!(get(&app, "never-written").await, json!({}));

    set(&app, "k0", json!("again")).await;
    assert_eq!(get(&app, "k0").await, json!({ "value": "again" }));
    app.call("store.delete", json!({ "key": "k0" }))
        .await
        .unwrap();
    assert_eq!(get(&app, "k0").await, json!({}), "deleted");
    app.call("store.delete", json!({ "key": "k0" }))
        .await
        .unwrap();
}

#[tokio::test]
async fn keys_come_in_order_and_by_prefix() {
    let root = tempfile::tempdir().unwrap();
    let app = app(root.path()).await;
    assert!(keys(&app, json!({})).await.is_empty());
    for key in ["b/2", "a/1", "b/1", "c", "ba"] {
        set(&app, key, json!(1)).await;
    }
    assert_eq!(
        keys(&app, json!({})).await,
        ["a/1", "b/1", "b/2", "ba", "c"]
    );
    assert_eq!(keys(&app, json!({ "prefix": "b/" })).await, ["b/1", "b/2"]);
    assert!(keys(&app, json!({ "prefix": "zzz" })).await.is_empty());
    app.call("store.delete", json!({ "key": "b/1" }))
        .await
        .unwrap();
    assert_eq!(keys(&app, json!({ "prefix": "b" })).await, ["b/2", "ba"]);
}

#[tokio::test]
async fn areas_are_apart_and_named_with_care() {
    let root = tempfile::tempdir().unwrap();
    let app = app(root.path()).await;
    set(&app, "k", json!("default")).await;
    for area in ["one", "two"] {
        app.call("store.open", json!({ "area": area }))
            .await
            .unwrap();
        assert!(keys(&app, json!({ "area": area })).await.is_empty());
        app.call(
            "store.set",
            json!({ "area": area, "key": "k", "value": area }),
        )
        .await
        .unwrap();
    }
    for area in ["one", "two"] {
        let reply = app
            .call("store.get", json!({ "area": area, "key": "k" }))
            .await
            .unwrap();
        assert_eq!(reply, json!({ "value": area }));
    }
    assert_eq!(get(&app, "k").await, json!({ "value": "default" }));
    let named = app
        .call("store.get", json!({ "area": "default", "key": "k" }))
        .await
        .unwrap();
    assert_eq!(
        named,
        json!({ "value": "default" }),
        "default is a name too"
    );
    for bad in ["", "a b", "a/b", "..", "ü", &"a".repeat(65)] {
        let error = app
            .call("store.open", json!({ "area": bad }))
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidArgument, "{bad:?}");
    }
}

#[tokio::test]
async fn keys_values_and_arguments_have_limits() {
    let root = tempfile::tempdir().unwrap();
    let app = app(root.path()).await;
    for key in ["".to_owned(), "k".repeat(1025)] {
        let error = app
            .call("store.set", json!({ "key": key, "value": 1 }))
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidArgument);
        for command in ["store.get", "store.delete"] {
            let error = app.call(command, json!({ "key": key })).await.unwrap_err();
            assert_eq!(error.code, ErrorCode::InvalidArgument, "{command}");
        }
    }
    set(&app, &"k".repeat(1024), json!(1)).await;

    let big = "x".repeat(256 * 1024);
    let error = app
        .call("store.set", json!({ "key": "big", "value": big }))
        .await
        .unwrap_err();
    assert_eq!(
        error.code,
        ErrorCode::InvalidArgument,
        "256 KiB and the quotes"
    );
    let almost = "x".repeat(256 * 1024 - 2);
    set(&app, "almost", json!(almost)).await;
    assert_eq!(
        get(&app, "almost").await["value"].as_str().unwrap().len(),
        almost.len()
    );

    for (command, args) in [
        ("store.set", json!({ "key": "k" })),
        ("store.get", json!({})),
        ("store.get", json!({ "key": "k", "extra": 1 })),
        ("store.keys", json!({ "prefix": "p".repeat(1025) })),
    ] {
        let error = app.call(command, args).await.unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidArgument, "{command}");
    }
}

#[tokio::test]
async fn what_is_kept_is_there_in_the_next_run_and_flush_asks_nothing_of_an_unused_store() {
    let root = tempfile::tempdir().unwrap();
    {
        let first = app(root.path()).await;
        first.call("store.flush", Value::Null).await.unwrap();
        set(&first, "kept", json!({ "n": 1 })).await;
        first
            .call(
                "store.set",
                json!({ "area": "other", "key": "kept", "value": "in other" }),
            )
            .await
            .unwrap();
        first.call("store.flush", Value::Null).await.unwrap();
    }
    let second = app(root.path()).await;
    assert_eq!(get(&second, "kept").await, json!({ "value": { "n": 1 } }));
    let other = second
        .call("store.get", json!({ "area": "other", "key": "kept" }))
        .await
        .unwrap();
    assert_eq!(other, json!({ "value": "in other" }));
}

#[tokio::test]
async fn an_application_sees_only_its_own_store_and_a_store_in_use_is_busy() {
    let (one, two) = (tempfile::tempdir().unwrap(), tempfile::tempdir().unwrap());
    let (a, b) = (app(one.path()).await, app(two.path()).await);
    set(&a, "k", json!("a")).await;
    set(&b, "k", json!("b")).await;
    assert_eq!(get(&a, "k").await, json!({ "value": "a" }));
    assert_eq!(get(&b, "k").await, json!({ "value": "b" }));

    let again = app(one.path()).await;
    let error = again
        .call("store.get", json!({ "key": "k" }))
        .await
        .unwrap_err();
    assert_eq!(
        error.code,
        ErrorCode::Busy,
        "another run of the application holds the store"
    );
}
