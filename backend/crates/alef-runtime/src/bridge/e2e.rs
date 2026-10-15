// SPDX-License-Identifier: MIT OR Apache-2.0
//! Commands for the end-to-end scenarios (`tests/e2e`); registered only with `ALEF_E2E=1`.
//! A scenario page drives the real transport inside Servo and reports its verdict on stderr.
use alef_core::{
    error::AlefError,
    registry::{command::Reply, dispatch::Registry, host::Host},
    security::permissions::Permission,
    session::BlockingExecutor,
};
use bytes::Bytes;
use serde::Deserialize;
use serde_json::Value;
use std::{
    num::NonZeroUsize,
    sync::{
        atomic::{AtomicUsize, Ordering},
        Arc,
    },
};

use crate::RuntimeHandle;

pub(crate) fn consent_hold_ms(e2e: Option<&str>, hold: Option<&str>) -> Option<u64> {
    if e2e != Some("1") {
        return None;
    }
    let ms = hold?;
    if ms.is_empty() || !ms.bytes().all(|byte| byte.is_ascii_digit()) {
        return None;
    }
    let ms = ms.parse::<u64>().ok()?;
    (1..=5000).contains(&ms).then_some(ms)
}

/// Delays the consent request of the end-to-end consent window. Called only from the
/// `runtime.consent.request` handler, so it runs only in the consent window process; an
/// application that fails the lookup with NOT_FOUND never reaches this code.
pub async fn e2e_hold_consent_request() {
    let e2e = std::env::var("ALEF_E2E").ok();
    let hold = std::env::var("ALEF_E2E_CONSENT_HOLD_MS").ok();
    let Some(ms) = consent_hold_ms(e2e.as_deref(), hold.as_deref()) else {
        return;
    };
    let started = std::time::Instant::now();
    eprintln!("ALEF_E2E consent-pending");
    tokio::time::sleep(std::time::Duration::from_millis(ms)).await;
    eprintln!(
        "ALEF_E2E consent-released elapsed_ms={}",
        started.elapsed().as_millis()
    );
}

#[cfg(test)]
mod consent_hold_tests {
    use super::consent_hold_ms;

    #[test]
    fn consent_hold_requires_exact_gate_and_bounded_duration() {
        assert_eq!(consent_hold_ms(Some("1"), Some("1")), Some(1));
        assert_eq!(consent_hold_ms(Some("1"), Some("5000")), Some(5000));
        for (e2e, ms) in [
            (None, Some("1")),
            (Some("true"), Some("1")),
            (Some("01"), Some("1")),
            (Some("1"), None),
            (Some("1"), Some("0")),
            (Some("1"), Some("5001")),
            (Some("1"), Some("-1")),
            (Some("1"), Some("+1")),
            (Some("1"), Some("1x")),
            (Some("1"), Some("18446744073709551616")),
        ] {
            assert_eq!(consent_hold_ms(e2e, ms), None, "{e2e:?} {ms:?}");
        }
    }
}

fn epoch_ms() -> u128 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis())
}

#[derive(Deserialize)]
struct Flood {
    total: usize,
    piece: usize,
}

#[derive(Deserialize)]
struct Target {
    target: String,
}

#[derive(Deserialize)]
struct Report {
    line: String,
}

#[derive(Deserialize)]
struct DropFiles {
    paths: Vec<String>,
}

#[derive(Deserialize)]
struct Once {
    path: String,
}

#[derive(Deserialize)]
struct Delay {
    ms: u64,
}

#[derive(Default)]
struct BusState {
    started: AtomicUsize,
    completed: AtomicUsize,
}

fn checked_ms(ms: u64) -> Result<u64, AlefError> {
    if (1..=5000).contains(&ms) {
        Ok(ms)
    } else {
        Err(AlefError::new(
            alef_core::ErrorCode::InvalidArgument,
            "ms must be between 1 and 5000",
        ))
    }
}

/// Registers `e2e.*` when the end-to-end mode is on.
pub(super) fn register(
    registry: &mut Registry,
    ui: &RuntimeHandle,
    enabled: bool,
    broken: bool,
) -> Result<(), AlefError> {
    if !enabled {
        return Ok(());
    }
    let executor = Arc::new(BlockingExecutor::new(NonZeroUsize::new(1).expect("one")));
    let state = Arc::new(BusState::default());
    let job_executor = executor.clone();
    let job_state = state.clone();
    registry
        .command::<Delay>("e2e.blockExecutor")?
        .handler(move |_, args| {
            let executor = job_executor.clone();
            let state = job_state.clone();
            async move {
                let ms = checked_ms(args.ms)?;
                let state_started = state.clone();
                let job = executor.submit(move || {
                    state_started.started.fetch_add(1, Ordering::SeqCst);
                    let worker = std::thread::current().id();
                    std::thread::sleep(std::time::Duration::from_millis(ms));
                    state_started.completed.fetch_add(1, Ordering::SeqCst);
                    worker
                })?;
                let worker = job.result().await?;
                Ok(Reply::Json(
                    serde_json::json!({"worker": format!("{worker:?}")}),
                ))
            }
        })?;
    let state_read = state.clone();
    registry
        .command::<Value>("e2e.busState")?
        .handler(move |_, _| {
            let state = state_read.clone();
            async move {
                Ok(Reply::Json(serde_json::json!({
                    "started": state.started.load(Ordering::SeqCst),
                    "completed": state.completed.load(Ordering::SeqCst),
                })))
            }
        })?;
    registry
        .command::<Delay>("e2e.blockBus")?
        .handler(|_, args| async move {
            let ms = checked_ms(args.ms)?;
            std::thread::sleep(std::time::Duration::from_millis(ms));
            Ok(Reply::Json(Value::Null))
        })?;
    let event_ui = ui.clone();
    let event_counter = Arc::new(AtomicUsize::new(0));
    registry
        .command::<Value>("e2e.busEvent")?
        .handler(move |ctx, _| {
            let ui = event_ui.clone();
            let counter = event_counter.clone();
            async move {
                let unique = format!(
                    "e2e-{}-{}",
                    ctx.session.window(),
                    counter.fetch_add(1, Ordering::SeqCst)
                );
                Host::emit(
                    &ui,
                    Some(ctx.session.window()),
                    "e2e.busEvent",
                    serde_json::json!({"id": unique}),
                );
                Ok(Reply::Json(serde_json::json!({"id": unique})))
            }
        })?;
    // Induced failure for proving that the runner detects a FAIL: corrupts every echo.
    registry
        .command::<Value>("e2e.echo")?
        .handler(move |ctx, args| async move {
            let body = ctx.body().cloned();
            Ok(match body {
                Some(body) => {
                    let mut bytes = body.to_vec();
                    if broken && !bytes.is_empty() {
                        bytes[0] ^= 0xff;
                    }
                    Reply::Bytes(Bytes::from(bytes))
                }
                None => Reply::Json(args),
            })
        })?;
    registry
        .command::<Flood>("e2e.flood")?
        .handler(|ctx, args| async move {
            let (writer, id) = ctx.streams().open_outgoing();
            let streams_ctx = ctx.clone();
            tokio::spawn(async move {
                let mut sent = 0;
                let mut peak = 0;
                while sent < args.total {
                    if writer
                        .send_binary(Bytes::from(vec![7u8; args.piece]))
                        .await
                        .is_err()
                    {
                        eprintln!(
                            "ALEF_E2E producer-stopped stream={id} sent={sent} at_ms={}",
                            epoch_ms()
                        );
                        return;
                    }
                    sent += args.piece;
                    peak = peak.max(streams_ctx.streams().outstanding(id).unwrap_or(0));
                }
                eprintln!("ALEF_E2E flood-done stream={id} sent={sent} peak_outstanding={peak}");
                writer.end();
            });
            Ok(Reply::Stream(id))
        })?;
    // Commands behind a permission: the dispatcher decides before the handler runs, so a reply
    // proves that the manifest allowed this target.
    for (name, permission) in [
        ("e2e.fsRead", Permission::FsRead),
        ("e2e.fsWrite", Permission::FsWrite),
        ("e2e.appEnv", Permission::AppEnv),
    ] {
        registry
            .command::<Target>(name)?
            .permission(permission, |args| Some(args.target.clone()))
            .handler(|_, _| async { Ok(Reply::Json(serde_json::json!({ "allowed": true }))) })?;
    }
    // True for the first caller only: creates the file, so a restarted program can tell it apart.
    registry
        .command::<Once>("e2e.once")?
        .handler(|_, once| async move {
            let created = std::fs::OpenOptions::new()
                .write(true)
                .create_new(true)
                .open(&once.path)
                .is_ok();
            Ok(Reply::Json(Value::Bool(created)))
        })?;
    // Files dropped on the window of the caller, as the system would drop them.
    let dropper = ui.clone();
    registry
        .command::<DropFiles>("e2e.dropFiles")?
        .handler(move |ctx, drop| {
            let ui = dropper.clone();
            async move {
                let paths = drop
                    .paths
                    .into_iter()
                    .map(std::path::PathBuf::from)
                    .collect();
                ui.simulate_drop(ctx.session.window(), paths).await?;
                Ok(Reply::Json(Value::Null))
            }
        })?;
    registry
        .command::<Report>("e2e.report")?
        .handler(|_, report| async move {
            eprintln!("ALEF_E2E {}", report.line);
            Ok(Reply::Json(Value::Null))
        })
}

#[cfg(test)]
mod registry_tests {
    use super::*;
    use alef_core::{
        protocol::call::Limits,
        registry::context::CallContext,
        session::{session::SessionManager, TokenSource},
        ErrorCode,
    };

    async fn context() -> CallContext {
        let tokens: TokenSource = Arc::new(|| "token".to_owned());
        let sessions = SessionManager::new(tokens, Limits::default());
        let session = sessions.begin_document(1).await;
        CallContext::new(
            session,
            super::super::closed_permissions().expect("closed permissions"),
        )
    }

    const COMMANDS: [&str; 4] = [
        "e2e.blockExecutor",
        "e2e.blockBus",
        "e2e.busState",
        "e2e.busEvent",
    ];

    #[test]
    fn registry_is_empty_when_disabled_and_commands_register_when_enabled() {
        let mut disabled = Registry::default();
        let (ui, _requests) = RuntimeHandle::channel();
        register(&mut disabled, &ui, false, false).expect("disabled registry");
        for name in COMMANDS {
            assert!(
                !disabled
                    .command_names()
                    .iter()
                    .any(|registered| registered == name),
                "{name}"
            );
        }

        let mut enabled = Registry::default();
        register(&mut enabled, &ui, true, false).expect("enabled registry");
        let names = enabled.command_names();
        for name in COMMANDS {
            assert!(names.iter().any(|registered| registered == name), "{name}");
        }
    }

    #[tokio::test]
    async fn invalid_durations_report_invalid_argument_for_both_commands() {
        let mut registry = Registry::default();
        let (ui, _requests) = RuntimeHandle::channel();
        register(&mut registry, &ui, true, false).expect("registry");
        let ctx = context().await;
        for command in ["e2e.blockExecutor", "e2e.blockBus"] {
            for args in [
                serde_json::json!({"ms": 0}),
                serde_json::json!({"ms": 5001}),
                serde_json::json!({"ms": -1}),
                serde_json::json!({"ms": "1"}),
            ] {
                let result = tokio::time::timeout(
                    std::time::Duration::from_secs(5),
                    registry.dispatch(command, ctx.clone(), args),
                )
                .await
                .unwrap_or_else(|_| panic!("{command} dispatch timed out"));
                assert_eq!(
                    result.expect_err("invalid duration must fail").code,
                    ErrorCode::InvalidArgument
                );
            }
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn executor_command_runs_on_a_different_thread_and_reports_state() {
        use alef_core::registry::command::Reply;

        let (ui, _requests) = RuntimeHandle::channel();
        let mut registry = Registry::default();
        register(&mut registry, &ui, true, false).expect("registry");
        let ctx = context().await;
        let caller = format!("{:?}", std::thread::current().id());
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            registry.dispatch(
                "e2e.blockExecutor",
                ctx.clone(),
                serde_json::json!({"ms":1}),
            ),
        )
        .await
        .expect("executor timed out")
        .expect("executor dispatch");
        let worker = match result {
            Reply::Json(value) => value["worker"]
                .as_str()
                .expect("worker thread id")
                .to_owned(),
            other => panic!("expected JSON reply, got {other:?}"),
        };
        assert_ne!(
            worker, caller,
            "executor must not run on the current-thread runtime"
        );

        let state = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            registry.dispatch("e2e.busState", ctx, serde_json::json!({})),
        )
        .await
        .expect("state dispatch timed out")
        .expect("state");
        assert!(
            matches!(state, Reply::Json(ref value) if value["started"] == 1 && value["completed"] == 1)
        );
    }
}
