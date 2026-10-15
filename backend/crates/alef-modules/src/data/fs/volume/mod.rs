// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native volume enumeration and deterministic snapshot comparison.
use super::meta::dto::{Volume, VolumeEvent, VolumeEventKind};
use alef_core::{AlefError, ErrorCode};
use std::num::NonZeroUsize;
use std::sync::OnceLock;

/// Local volume-operation budget: at most eight queued or running short jobs.
/// This is not a global or per-session framework limit.
fn executor() -> &'static alef_core::session::executor::BlockingExecutor {
    static EXECUTOR: OnceLock<alef_core::session::executor::BlockingExecutor> = OnceLock::new();
    EXECUTOR.get_or_init(|| {
        alef_core::session::executor::BlockingExecutor::new(
            NonZeroUsize::new(8).expect("non-zero volume job budget"),
        )
    })
}

#[cfg(target_os = "linux")]
#[path = "linux.rs"]
mod native;
#[cfg(target_os = "macos")]
#[path = "macos.rs"]
mod native;
#[cfg(windows)]
#[path = "windows.rs"]
mod native;
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
mod native {
    use super::*;
    pub(super) fn list() -> Result<Vec<Volume>, AlefError> {
        Err(super::super::meta::fault::coded(ErrorCode::NotAvailable))
    }
}

#[cfg(any(windows, test))]
pub(super) fn drive_type(value: u32) -> super::meta::dto::VolumeKind {
    use super::meta::dto::VolumeKind;
    match value {
        2 => VolumeKind::Removable,
        3 => VolumeKind::Fixed,
        4 => VolumeKind::Network,
        5 => VolumeKind::Optical,
        6 => VolumeKind::Ram,
        _ => VolumeKind::Unknown,
    }
}

pub(super) fn list() -> Result<Vec<Volume>, AlefError> {
    native::list()
}

/// True when at least one manifest read right grants real access.
pub(super) fn gate(
    rights: &[alef_core::security::consent::Right],
    consent: &alef_core::security::consent::Consent,
) -> bool {
    use alef_core::security::consent::Decision;
    rights
        .iter()
        .any(|right| right.permission == "fs.read" && consent.decision(right) == Decision::Allow)
}

pub(super) fn register(
    registry: &mut alef_core::registry::dispatch::Registry,
) -> Result<(), AlefError> {
    registry
        .command::<()>("fs.volumes")?
        .handler(|ctx, ()| async move {
            if ctx.cancel().is_cancelled() || !ctx.session.is_open() {
                return Err(AlefError::new(ErrorCode::Closed, "call cancelled"));
            }
            let permissions = ctx.permissions.clone();
            let job = executor().submit(move || {
                let rights = permissions.rights();
                let consent = permissions.consent();
                if gate(&rights, &consent) {
                    list()
                } else {
                    Ok(Vec::new())
                }
            })?;
            let volumes = job.result().await??;
            if ctx.cancel().is_cancelled() || !ctx.session.is_open() {
                return Err(AlefError::new(ErrorCode::Closed, "call cancelled"));
            }
            crate::json(&volumes)
        })?;
    registry
        .command::<()>("fs.watchVolumes")?
        .handler(|ctx, ()| async move {
            if ctx.cancel().is_cancelled() || !ctx.session.is_open() {
                return Err(AlefError::new(ErrorCode::Closed, "call cancelled"));
            }
            let permissions = ctx.permissions.clone();
            let job =
                executor().submit(move || gate(&permissions.rights(), &permissions.consent()))?;
            let allowed = job.result().await?;
            if ctx.cancel().is_cancelled() || !ctx.session.is_open() {
                return Err(AlefError::new(ErrorCode::Closed, "call cancelled"));
            }
            if allowed {
                watch::start(&ctx).await
            } else {
                // An empty substitute stream ends immediately; it cannot hang waiting for a task
                // or lease that is intentionally not created for a denied/substituted watch.
                let (writer, id) = ctx.streams().open_outgoing();
                writer.end();
                crate::json(&serde_json::json!({ "stream": id.0 }))
            }
        })?;
    Ok(())
}

mod watch;

/// Other metadata changes are reported; free-space fluctuations alone are ignored.
pub(super) fn diff(before: &[Volume], after: &[Volume]) -> Vec<VolumeEvent> {
    use std::collections::BTreeMap;
    let before: BTreeMap<_, _> = before.iter().map(|v| (v.root.as_str(), v)).collect();
    let after: BTreeMap<_, _> = after.iter().map(|v| (v.root.as_str(), v)).collect();
    let roots: std::collections::BTreeSet<_> = before.keys().chain(after.keys()).copied().collect();
    roots
        .into_iter()
        .filter_map(|root| match (before.get(root), after.get(root)) {
            (None, Some(v)) => Some(VolumeEvent {
                r#type: VolumeEventKind::Mount,
                volume: (**v).clone(),
            }),
            (Some(v), None) => Some(VolumeEvent {
                r#type: VolumeEventKind::Unmount,
                volume: (**v).clone(),
            }),
            (Some(old), Some(new))
                if old.label != new.label
                    || old.kind != new.kind
                    || old.file_system != new.file_system
                    || old.total != new.total
                    || old.read_only != new.read_only =>
            {
                Some(VolumeEvent {
                    r#type: VolumeEventKind::Change,
                    volume: (**new).clone(),
                })
            }
            _ => None,
        })
        .collect()
}

#[cfg(test)]
#[path = "tests.rs"]
mod tests;
