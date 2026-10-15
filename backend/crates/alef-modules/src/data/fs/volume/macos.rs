// SPDX-License-Identifier: MIT OR Apache-2.0
use super::super::meta::dto::{Volume, VolumeKind};
use alef_core::AlefError;
use std::sync::Mutex;

static MOUNTS: Mutex<()> = Mutex::new(());

fn bounded(bytes: &[libc::c_char]) -> String {
    let end = bytes
        .iter()
        .position(|byte| *byte == 0)
        .unwrap_or(bytes.len());
    String::from_utf8_lossy(
        &bytes[..end]
            .iter()
            .map(|byte| *byte as u8)
            .collect::<Vec<_>>(),
    )
    .into_owned()
}

fn is_internal_mount(mount: &str, fs: &str) -> bool {
    (mount.starts_with("/System/Volumes/") && mount != "/System/Volumes/Data")
        || matches!(fs, "devfs" | "autofs" | "procfs" | "fdesc" | "nullfs")
}

fn prefer_data_root(volumes: &mut Vec<Volume>, data: Volume) {
    volumes.retain(|volume| volume.root != "/");
    let mut data = data;
    data.root = "/".to_owned();
    volumes.push(data);
}

pub(super) fn list() -> Result<Vec<Volume>, AlefError> {
    let _guard = MOUNTS
        .lock()
        .map_err(|_| super::super::meta::fault::coded(alef_core::ErrorCode::NotAvailable))?;
    let mut table = std::ptr::null_mut();
    // SAFETY: the mutex serializes this module's getmntinfo calls; successful count bounds the static initialized entries, copied before unlocking.
    let count = unsafe { libc::getmntinfo(&mut table, libc::MNT_NOWAIT) };
    if count <= 0 || table.is_null() {
        return Err(super::super::meta::fault::coded(
            alef_core::ErrorCode::NotAvailable,
        ));
    }
    let mut result = Vec::new();
    let mut data_root = None;
    for i in 0..count as usize {
        // SAFETY: getmntinfo guarantees `count` initialized entries at `table` on positive return.
        let stat = unsafe { &*table.add(i) };
        let mount = bounded(&stat.f_mntonname);
        let fs = bounded(&stat.f_fstypename);
        if is_internal_mount(&mount, &fs) {
            continue;
        }
        let block = u64::try_from(stat.f_bsize).ok().filter(|b| *b != 0);
        let total = block.and_then(|b| {
            u64::try_from(stat.f_blocks)
                .ok()
                .and_then(|n| n.checked_mul(b))
        });
        let free = block.and_then(|b| {
            u64::try_from(stat.f_bavail)
                .ok()
                .and_then(|n| n.checked_mul(b))
        });
        let kind = if stat.f_flags & libc::MNT_LOCAL == 0 {
            VolumeKind::Network
        } else if matches!(fs.as_str(), "udf" | "cd9660") {
            VolumeKind::Optical
        } else if matches!(fs.as_str(), "tmpfs" | "ramfs") {
            VolumeKind::Ram
        } else if matches!(fs.as_str(), "apfs" | "hfs" | "msdos" | "exfat") {
            VolumeKind::Fixed
        } else {
            VolumeKind::Unknown
        };
        let volume = Volume {
            root: mount.clone(),
            label: None,
            kind,
            file_system: Some(fs),
            total,
            free,
            read_only: stat.f_flags & libc::MNT_RDONLY != 0,
        };
        if mount == "/System/Volumes/Data" {
            data_root = Some(volume);
        } else {
            result.push(volume);
        }
    }
    if let Some(data) = data_root {
        prefer_data_root(&mut result, data);
    }
    result.sort_by(|a, b| a.root.cmp(&b.root));
    result.dedup_by(|a, b| a.root == b.root);
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn internal_mount_filter_skips_system_subvolumes_and_pseudo_filesystems() {
        assert!(is_internal_mount("/System/Volumes/Preboot", "apfs"));
        assert!(is_internal_mount("/dev", "devfs"));
        assert!(!is_internal_mount("/System/Volumes/Data", "apfs"));
        assert!(!is_internal_mount("/Volumes/USB", "apfs"));
    }
    #[test]
    fn data_volume_replaces_existing_root_for_merged_finder_view() {
        let mut volumes = vec![Volume {
            root: "/".into(),
            label: None,
            kind: VolumeKind::Fixed,
            file_system: Some("apfs".into()),
            total: Some(1),
            free: Some(1),
            read_only: false,
        }];
        let data = Volume {
            root: "/System/Volumes/Data".into(),
            label: None,
            kind: VolumeKind::Fixed,
            file_system: Some("apfs".into()),
            total: Some(2),
            free: Some(2),
            read_only: false,
        };
        prefer_data_root(&mut volumes, data);
        assert_eq!(volumes.len(), 1);
        assert_eq!(volumes[0].root, "/");
        assert_eq!(volumes[0].total, Some(2));
    }
}
