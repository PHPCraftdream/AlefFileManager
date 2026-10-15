// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native volume enumeration and deterministic snapshot comparison.
use super::meta::dto::{Volume, VolumeEvent, VolumeEventKind};
use alef_core::AlefError;
#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
use alef_core::ErrorCode;

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
            let permissions = ctx.permissions.clone();
            let volumes = super::blocking(move || {
                let rights = permissions.rights();
                let consent = permissions.consent();
                if gate(&rights, &consent) {
                    list()
                } else {
                    Ok(Vec::new())
                }
            })
            .await?;
            crate::json(&volumes)
        })?;
    registry
        .command::<()>("fs.watchVolumes")?
        .handler(|ctx, ()| async move {
            let permissions = ctx.permissions.clone();
            let allowed =
                super::blocking(move || Ok(gate(&permissions.rights(), &permissions.consent())))
                    .await?;
            if allowed {
                watch::start(&ctx).await
            } else {
                let (writer, id) = ctx.streams().open_outgoing();
                tokio::spawn(async move {
                    writer.closed().await;
                });
                crate::json(&serde_json::json!({ "stream": id.0 }))
            }
        })?;
    Ok(())
}

mod watch;

/// Free-space-only fluctuations are intentionally ignored; other metadata changes are reported.
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
