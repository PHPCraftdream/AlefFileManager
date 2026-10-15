// SPDX-License-Identifier: MIT OR Apache-2.0
use super::super::meta::dto::{Volume, VolumeKind};
use alef_core::AlefError;
use std::{ffi::OsStr, fs, os::unix::ffi::OsStrExt, path::Path};

const PSEUDO: &[&str] = &[
    "proc",
    "sysfs",
    "cgroup",
    "cgroup2",
    "devtmpfs",
    "devpts",
    "securityfs",
    "debugfs",
    "tracefs",
    "configfs",
    "pstore",
    "mqueue",
    "hugetlbfs",
    "fusectl",
    "rpc_pipefs",
    "autofs",
    "nsfs",
    "binfmt_misc",
];

pub(super) fn decode_mount_field(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes.get(i) == Some(&b'\\') && i + 3 < bytes.len() {
            let decoded = match &bytes[i + 1..i + 4] {
                b"040" => Some(b' '),
                b"011" => Some(b'\t'),
                b"012" => Some(b'\n'),
                b"134" => Some(b'\\'),
                _ => None,
            };
            if let Some(byte) = decoded {
                out.push(byte);
                i += 4;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    out
}

pub(super) fn pseudo(fs: &str, mount: &[u8]) -> bool {
    if PSEUDO.contains(&fs) {
        return true;
    }
    if fs == "tmpfs" {
        let p = Path::new(OsStr::from_bytes(mount));
        return !(p.starts_with("/media") || p.starts_with("/run/media"));
    }
    fs == "overlay" && mount != b"/"
}

fn kind(fs: &str) -> VolumeKind {
    if matches!(
        fs,
        "nfs" | "nfs4" | "cifs" | "smb3" | "sshfs" | "fuse.sshfs" | "9p" | "ceph" | "glusterfs"
    ) {
        VolumeKind::Network
    } else if matches!(fs, "iso9660" | "udf") {
        VolumeKind::Optical
    } else if matches!(fs, "tmpfs" | "ramfs") {
        VolumeKind::Ram
    } else if [
        "ext", "ext2", "ext3", "ext4", "btrfs", "xfs", "vfat", "exfat", "ntfs", "apfs",
    ]
    .contains(&fs)
    {
        VolumeKind::Fixed
    } else {
        VolumeKind::Unknown
    }
}

pub(super) fn parse_mountinfo(input: &[u8]) -> Vec<(Vec<u8>, String, Vec<u8>, bool)> {
    let mut entries = Vec::new();
    for line in input.split(|b| *b == b'\n') {
        let Some(separator) = line.windows(3).position(|window| window == b" - ") else {
            continue;
        };
        let left = &line[..separator];
        let right = &line[separator + 3..];
        let left: Vec<_> = left
            .split(|b| *b == b' ')
            .filter(|x| !x.is_empty())
            .collect();
        let right: Vec<_> = right
            .split(|b| *b == b' ')
            .filter(|x| !x.is_empty())
            .collect();
        if left.len() < 6 || right.len() < 3 {
            continue;
        }
        let fs = String::from_utf8_lossy(right[0]).into_owned();
        let mount = decode_mount_field(left[4]);
        if pseudo(&fs, &mount) {
            continue;
        }
        let opts = left[5].split(|b| *b == b',').any(|o| o == b"ro");
        entries.push((mount, fs, decode_mount_field(right[1]), opts));
    }
    entries
}

pub(super) fn list() -> Result<Vec<Volume>, AlefError> {
    let bytes =
        fs::read("/proc/self/mountinfo").map_err(|e| super::super::meta::fault::fault(e))?;
    let mut volumes = Vec::new();
    for (root, fs_type, _source, mount_ro) in parse_mountinfo(&bytes) {
        let c_path = std::ffi::CString::new(root.as_slice())
            .map_err(|_| super::super::meta::fault::coded(alef_core::ErrorCode::NotAvailable))?;
        let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
        // SAFETY: `statvfs` writes a complete statvfs value on success; the C path is NUL-terminated and output storage is writable.
        let result = unsafe { libc::statvfs(c_path.as_ptr(), stat.as_mut_ptr()) };
        let (total, free, ro) = if result == 0 {
            // SAFETY: a successful statvfs call fully initialized the output structure.
            let stat = unsafe { stat.assume_init() };
            let block = u64::try_from(stat.f_frsize).ok().filter(|b| *b != 0);
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
            (total, free, mount_ro || stat.f_flag & libc::ST_RDONLY != 0)
        } else {
            (None, None, mount_ro)
        };
        volumes.push(Volume {
            root: String::from_utf8_lossy(&root).into_owned(),
            label: None,
            kind: kind(&fs_type),
            file_system: Some(fs_type),
            total,
            free,
            read_only: ro,
        });
    }
    volumes.sort_by(|a, b| a.root.cmp(&b.root));
    volumes.dedup_by(|a, b| a.root == b.root);
    Ok(volumes)
}
