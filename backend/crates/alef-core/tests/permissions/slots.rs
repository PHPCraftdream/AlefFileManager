// SPDX-License-Identifier: MIT OR Apache-2.0
//! A call that waits on the outside does not use up the slots of the calls that run.
use super::{build, closed};
use alef_core::{
    protocol::Limits,
    registry::{command::Reply, context::CallContext, dispatch::Registry},
    security::{
        consent::{Consent, Decision, Right},
        permissions::Permission,
    },
    session::SessionManager,
    ErrorCode,
};
use serde_json::{json, Value};
use std::{future::Future, sync::Arc, time::Duration};
use tokio::sync::Semaphore;

async fn bounded<T>(future: impl Future<Output = T>) -> T {
    tokio::time::timeout(Duration::from_secs(10), future)
        .await
        .expect("in time")
}

#[tokio::test]
async fn a_stand_in_that_waits_leaves_the_running_calls_and_a_real_call_stays_among_them() {
    let mut policy = closed();
    policy.app.env = vec!["HOME".into(), "PATH".into()];
    let mut consent = Consent::undecided();
    consent.set(Right::scoped("app.env", "HOME"), Decision::Allow);
    consent.set(Right::scoped("app.env", "PATH"), Decision::Substitute);
    let permissions = Arc::new(build(&policy).with_consent(consent));
    #[derive(serde::Deserialize)]
    struct Name {
        name: String,
    }
    let entered = Arc::new(Semaphore::new(0));
    let release = Arc::new(Semaphore::new(0));
    let mut registry = Registry::default();
    let (e, r) = (entered.clone(), release.clone());
    registry
        .command::<Name>("probe.hold")
        .unwrap()
        .permission(Permission::AppEnv, |args| Some(args.name.clone()))
        .substitutes()
        .handler(move |_, _| {
            let (e, r) = (e.clone(), r.clone());
            async move {
                e.add_permits(1);
                r.acquire().await.unwrap().forget();
                Ok(Reply::Json(Value::Null))
            }
        })
        .unwrap();
    let limits = Limits {
        max_concurrent_calls_per_session: 1,
        max_waiting_calls_per_session: 1,
        ..Limits::default()
    };
    let session = SessionManager::new(Arc::new(|| "t".to_owned()), limits)
        .begin_document(1)
        .await;
    // Like the transport, the test keeps the slot of a call until the call has ended.
    let context = || {
        let permit = Arc::new(session.try_begin_call().expect("a slot"));
        let ctx =
            CallContext::new(session.clone(), permissions.clone()).with_permit(permit.clone());
        (ctx, permit)
    };
    let start = |name: &'static str| {
        let ((ctx, permit), registry) = (context(), registry.clone());
        let task = tokio::spawn(async move {
            registry
                .dispatch("probe.hold", ctx, json!({ "name": name }))
                .await
        });
        (task, permit)
    };

    // A stand-in waits as long as the real thing would: it takes no slot of the running calls.
    let (stand_in, stand_in_slot) = start("PATH");
    bounded(entered.acquire()).await.unwrap().forget();
    drop(
        session
            .try_begin_call()
            .expect("the running budget is free"),
    );
    // The budget of waiting calls is full: another stand-in is refused before it starts.
    let (ctx, refused_slot) = context();
    let refused = bounded(registry.dispatch("probe.hold", ctx, json!({ "name": "PATH" })))
        .await
        .unwrap_err();
    assert_eq!(refused.code, ErrorCode::Busy);
    assert_eq!(entered.available_permits(), 0, "it never started");
    drop(refused_slot);
    release.add_permits(1);
    bounded(stand_in).await.unwrap().unwrap();
    drop(stand_in_slot);

    // A real call holds its slot until it ends.
    let (real, real_slot) = start("HOME");
    bounded(entered.acquire()).await.unwrap().forget();
    assert_eq!(
        session.try_begin_call().unwrap_err().code,
        ErrorCode::Busy,
        "a call that does the real thing is a running call"
    );
    release.add_permits(1);
    bounded(real).await.unwrap().unwrap();
    drop(real_slot);
    drop(session.try_begin_call().expect("released"));
}
