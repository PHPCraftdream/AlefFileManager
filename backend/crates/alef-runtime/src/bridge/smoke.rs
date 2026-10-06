// SPDX-License-Identifier: MIT OR Apache-2.0
//! Commands of the M1 smoke page (`experiments/m1-smoke`); registered only with `ALEF_M1_SMOKE=1`.
//! The page drives the real transport inside Servo and reports a verdict on stderr.
use alef_core::{
    error::AlefError,
    registry::{command::Reply, dispatch::Registry},
};
use bytes::Bytes;
use serde::Deserialize;
use serde_json::Value;

use crate::RuntimeHandle;

fn enabled() -> bool {
    std::env::var("ALEF_M1_SMOKE").is_ok_and(|value| value == "1")
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
struct Report {
    line: String,
}

/// Registers `smoke.*` when the smoke mode is on.
pub(super) fn register(registry: &mut Registry, _ui: &RuntimeHandle) -> Result<(), AlefError> {
    if !enabled() {
        return Ok(());
    }
    // Induced failure for proving that the runner detects a FAIL: corrupts every echo.
    let broken = std::env::var("ALEF_M1_SMOKE_BREAK").is_ok_and(|value| value == "1");
    registry
        .command::<Value>("smoke.echo")?
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
        .command::<Flood>("smoke.flood")?
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
                            "M1_SMOKE producer-stopped stream={id} sent={sent} at_ms={}",
                            epoch_ms()
                        );
                        return;
                    }
                    sent += args.piece;
                    peak = peak.max(streams_ctx.streams().outstanding(id).unwrap_or(0));
                }
                eprintln!("M1_SMOKE flood-done stream={id} sent={sent} peak_outstanding={peak}");
                writer.end();
            });
            Ok(Reply::Stream(id))
        })?;
    registry
        .command::<Report>("smoke.report")?
        .handler(|_, report| async move {
            eprintln!("M1_SMOKE {}", report.line);
            Ok(Reply::Json(Value::Null))
        })
}
