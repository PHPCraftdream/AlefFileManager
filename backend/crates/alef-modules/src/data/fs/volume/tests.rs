// SPDX-License-Identifier: MIT OR Apache-2.0
use super::super::meta::dto::{Volume, VolumeEventKind, VolumeKind};
use alef_core::security::consent::{Consent, Decision, Right};

#[test]
fn watcher_gate_requires_a_real_allowed_read_right() {
    let read = Right::scoped("fs.read", "/home/user/**");
    let write = Right::scoped("fs.write", "/home/user/**");
    assert!(!super::gate(&[], &Consent::allow_all()));
    assert!(!super::gate(&[write], &Consent::allow_all()));
    let mut substitute = Consent::undecided();
    substitute.set(read.clone(), Decision::Substitute);
    assert!(!super::gate(&[read.clone()], &substitute));
    let mut denied = Consent::undecided();
    denied.set(read.clone(), Decision::Deny);
    assert!(!super::gate(&[read.clone()], &denied));
    let denied_other = Right::scoped("fs.read", "/documents/**");
    let mut mixed = Consent::undecided();
    mixed.set(read.clone(), Decision::Allow);
    mixed.set(denied_other.clone(), Decision::Deny);
    assert!(super::gate(&[read, denied_other], &mixed));
}

use serde_json::Value;

fn volume(root: &str) -> Volume {
    Volume {
        root: root.into(),
        label: None,
        kind: VolumeKind::Fixed,
        file_system: None,
        total: None,
        free: None,
        read_only: false,
    }
}

#[test]
fn diff_reports_sorted_mount_unmount_and_change_with_exact_payloads() {
    let before = [volume("/b"), volume("/gone")];
    let mut changed = volume("/b");
    changed.read_only = true;
    let mounted = volume("/a");
    let after = [mounted.clone(), changed.clone()];
    let events = super::diff(&before, &after);
    assert_eq!(
        events
            .iter()
            .map(|e| e.volume.root.as_str())
            .collect::<Vec<_>>(),
        ["/a", "/b", "/gone"]
    );
    assert_eq!(
        events[0],
        super::VolumeEvent {
            r#type: VolumeEventKind::Mount,
            volume: mounted
        }
    );
    assert_eq!(
        events[1],
        super::VolumeEvent {
            r#type: VolumeEventKind::Change,
            volume: changed
        }
    );
    assert_eq!(
        events[2],
        super::VolumeEvent {
            r#type: VolumeEventKind::Unmount,
            volume: volume("/gone")
        }
    );
}

#[test]
fn diff_ignores_unchanged_snapshots_and_free_space_only_fluctuations() {
    let old = volume("/");
    assert!(super::diff(&[old.clone()], &[old.clone()]).is_empty());
    let mut changed = old.clone();
    changed.free = Some(3);
    assert!(super::diff(&[old], &[changed]).is_empty());
}

#[test]
fn diff_reports_each_meaningful_optional_or_fixed_metadata_change() {
    let base = volume("/");
    let mut variants = Vec::new();
    let mut label = base.clone();
    label.label = Some("disk".into());
    variants.push(label);
    let mut kind = base.clone();
    kind.kind = VolumeKind::Removable;
    variants.push(kind);
    let mut file_system = base.clone();
    file_system.file_system = Some("ext4".into());
    variants.push(file_system);
    let mut total = base.clone();
    total.total = Some(12);
    variants.push(total);
    let mut read_only = base.clone();
    read_only.read_only = true;
    variants.push(read_only);
    for expected in variants {
        let events = super::diff(&[base.clone()], &[expected.clone()]);
        assert_eq!(
            events,
            [super::VolumeEvent {
                r#type: VolumeEventKind::Change,
                volume: expected
            }]
        );
    }
}

#[test]
fn volume_event_serialization_uses_type_file_system_and_read_only_wire_names() {
    let mut mounted = volume("/");
    mounted.file_system = Some("ext4".into());
    mounted.read_only = true;
    let value: Value = serde_json::to_value(super::VolumeEvent {
        r#type: VolumeEventKind::Mount,
        volume: mounted,
    })
    .expect("serialize DTO");
    assert_eq!(value["type"], "mount");
    assert!(value.get("kind").is_none());
    assert_eq!(value["volume"]["fileSystem"], "ext4");
    assert_eq!(value["volume"]["readOnly"], true);
    assert!(value["volume"].get("file_system").is_none());
    let mut absent = volume("/");
    absent.read_only = false;
    let value = serde_json::to_value(super::VolumeEvent {
        r#type: VolumeEventKind::Unmount,
        volume: absent,
    })
    .expect("serialize DTO");
    assert_eq!(value["type"], "unmount");
    assert!(value["volume"].get("label").is_none());
    assert!(value["volume"].get("fileSystem").is_none());
    assert!(value["volume"].get("total").is_none());
    assert!(value["volume"].get("free").is_none());
}

#[cfg(target_os = "linux")]
#[test]
fn mountinfo_decodes_only_supported_octal_escapes_once() {
    use super::native::decode_mount_field;
    assert_eq!(
        decode_mount_field(b"a\\040b\\011c\\012d\\134e"),
        b"a b\tc\nd\\e"
    );
    assert_eq!(decode_mount_field(b"\\040"), b" ");
    assert_eq!(decode_mount_field(b"\\134040"), b"\\040");
    assert_eq!(decode_mount_field(b"\\777\\04"), b"\\777\\04");
}

#[cfg(target_os = "linux")]
#[test]
fn mountinfo_splits_only_exact_separator_and_maps_fields_escapes_and_options() {
    use super::native::parse_mountinfo;
    let data = b"bad line\n1 0 0:1 / / rw - ext4 /dev/root rw\n2 0 0:2 / /mnt/disk-name\\040one ro - nfs4 host:/export rw\n3 0 0:3 / /run/media/usb\\377 rw - tmpfs /dev/nonutf8\\377 rw\n4 0 0:4 / /media/escaped\\134040 rw - ext4 /dev/a rw\n5 0 0:5 / /truncated rw - ext4\n";
    let parsed = parse_mountinfo(data);
    assert_eq!(parsed.len(), 4);
    assert_eq!(
        parsed[0],
        (b"/".to_vec(), "ext4".into(), b"/dev/root".to_vec(), false)
    );
    assert_eq!(parsed[1].0, b"/mnt/disk-name one");
    assert_eq!(parsed[1].1, "nfs4");
    assert_eq!(parsed[1].2, b"host:/export");
    assert!(parsed[1].3);
    assert_eq!(parsed[2].0, b"/run/media/usb\\377");
    assert_eq!(parsed[2].2, b"/dev/nonutf8\\377");
    assert_eq!(parsed[3].0, b"/media/escaped\\040");
}

#[cfg(target_os = "linux")]
#[test]
fn linux_pseudo_filter_covers_kernel_mounts_tmpfs_boundaries_and_overlay_root() {
    use super::native::pseudo;
    for fs in [
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
    ] {
        assert!(pseudo(fs, b"/anything"), "{fs}");
    }
    assert!(pseudo("tmpfs", b"/tmp"));
    assert!(!pseudo("tmpfs", b"/media/disk"));
    assert!(!pseudo("tmpfs", b"/run/media/user/disk"));
    assert!(pseudo("tmpfs", b"/mediabad"));
    assert!(pseudo("overlay", b"/container"));
    assert!(!pseudo("overlay", b"/"));
}

#[cfg(test)]
#[test]
fn windows_drive_type_values_map_every_documented_category_on_any_test_target() {
    assert!(matches!(super::drive_type(0), VolumeKind::Unknown));
    assert!(matches!(super::drive_type(1), VolumeKind::Unknown));
    assert!(matches!(super::drive_type(2), VolumeKind::Removable));
    assert!(matches!(super::drive_type(3), VolumeKind::Fixed));
    assert!(matches!(super::drive_type(4), VolumeKind::Network));
    assert!(matches!(super::drive_type(5), VolumeKind::Optical));
    assert!(matches!(super::drive_type(6), VolumeKind::Ram));
    assert!(matches!(super::drive_type(99), VolumeKind::Unknown));
}
