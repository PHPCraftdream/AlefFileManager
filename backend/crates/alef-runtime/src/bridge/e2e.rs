// SPDX-License-Identifier: MIT OR Apache-2.0
//! Commands for the end-to-end scenarios (`tests/e2e`); registered only with `ALEF_E2E=1`.
//! A scenario page drives the real transport inside Servo and reports its verdict on stderr.
use alef_core::{
    error::AlefError,
    registry::{command::Reply, dispatch::Registry},
    security::permissions::Permission,
};
use bytes::Bytes;
use serde::Deserialize;
use serde_json::Value;

use crate::RuntimeHandle;

fn enabled() -> bool {
    std::env::var("ALEF_E2E").is_ok_and(|value| value == "1")
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

/// Registers `e2e.*` when the end-to-end mode is on.
pub(super) fn register(registry: &mut Registry, _ui: &RuntimeHandle) -> Result<(), AlefError> {
    if !enabled() {
        return Ok(());
    }
    // Induced failure for proving that the runner detects a FAIL: corrupts every echo.
    let broken = std::env::var("ALEF_E2E_BREAK").is_ok_and(|value| value == "1");
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
    registry
        .command::<Report>("e2e.report")?
        .handler(|_, report| async move {
            eprintln!("ALEF_E2E {}", report.line);
            Ok(Reply::Json(Value::Null))
        })
}
