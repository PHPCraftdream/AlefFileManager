// SPDX-License-Identifier: MIT OR Apache-2.0
use super::{CrashInfo, CrashReport, Level, LogStore, ReadOptions, RuntimeInfo};
use alef_core::ErrorCode;
use serde_json::{json, Value};
use std::{
    fs,
    sync::mpsc,
    thread,
    time::{Duration, Instant},
};
#[cfg(test)]
fn valid_rfc3339_millis(value: &str) -> bool {
    let b = value.as_bytes();
    let len_ok = b.len() == 20 || b.len() == 24;
    len_ok
        && b[4] == b'-'
        && b[7] == b'-'
        && b[10] == b'T'
        && b[13] == b':'
        && b[16] == b':'
        && (b.len() == 20 || (b[19] == b'.' && b[23] == b'Z'))
        && b[b.len() - 1] == b'Z'
        && b.iter()
            .enumerate()
            .all(|(i, c)| [4, 7, 10, 13, 16, 19, 23].contains(&i) || c.is_ascii_digit())
        && (1..=12).contains(&value[5..7].parse::<u8>().unwrap_or(0))
        && (1..=31).contains(&value[8..10].parse::<u8>().unwrap_or(0))
        && value[11..13].parse::<u8>().is_ok_and(|v| v < 24)
        && value[14..16].parse::<u8>().is_ok_and(|v| v < 60)
        && value[17..19].parse::<u8>().is_ok_and(|v| v < 60)
}

#[test]
fn crash_missing_report_uses_a_valid_timestamp_shaped_id() {
    let (d, s) = store();
    fs::create_dir_all(d.path().join("crashes")).unwrap();
    s.save_crash("00000000000000000002-existing", "existing")
        .unwrap();
    let e = s.crash("00000000000000000001-missing").unwrap_err();
    assert_eq!(e.message, "crash report not found");
}
fn store() -> (tempfile::TempDir, LogStore) {
    let d = tempfile::tempdir().unwrap();
    let s = LogStore::new(d.path()).unwrap();
    (d, s)
}
fn at(b: Instant, ms: u64) -> Instant {
    b + Duration::from_millis(ms)
}
#[test]
fn constructor_is_lazy_and_storage_paths_are_separate() {
    let d = tempfile::tempdir().unwrap();
    let root = d.path().join("missing");
    let s = LogStore::new(&root).unwrap();
    assert!(!root.exists());
    s.write(Level::Info, "x", Value::Null).unwrap();
    assert!(root.join("logs/app.log").exists());
    assert!(!root.join("crashes").exists());
}
#[test]
fn production_cap_rotation_keeps_five_newest_files_under_one_mebibyte() {
    let d = tempfile::tempdir().unwrap();
    let s = LogStore::with_test_cap(d.path().to_path_buf(), 1024 * 1024).unwrap();
    let b = Instant::now();
    for i in 0..8000 {
        s.write_at(
            Level::Info,
            &format!("{i}:{}", "x".repeat(800)),
            Value::Null,
            at(b, (i / 200 + 1) * 1000),
        )
        .unwrap();
    }
    let dir = s.log_dir();
    let mut count = 0;
    for n in 0..=4 {
        let p = dir.join(if n == 0 {
            "app.log".into()
        } else {
            format!("app.log.{n}")
        });
        if p.exists() {
            count += 1;
            assert!(fs::metadata(&p).unwrap().len() <= 1024 * 1024);
        }
    }
    assert_eq!(count, 5);
    let r = s
        .read(ReadOptions {
            limit: 10,
            level: None,
        })
        .unwrap();
    assert_eq!(
        r.records
            .iter()
            .map(|r| r.message.split(':').next().unwrap())
            .collect::<Vec<_>>(),
        (7990..8000).map(|i| i.to_string()).collect::<Vec<_>>()
    );
    let oldest = fs::read_to_string(dir.join("app.log.4")).unwrap();
    let first: super::LogRecord = serde_json::from_str(oldest.lines().next().unwrap()).unwrap();
    assert!(
        first
            .message
            .split(':')
            .next()
            .unwrap()
            .parse::<usize>()
            .unwrap()
            > 0
    );
}
#[test]
fn reading_newest_records_across_rotated_files_is_chronological() {
    let d = tempfile::tempdir().unwrap();
    let s = LogStore::with_test_cap(d.path().to_path_buf(), 64 * 1024).unwrap();
    let b = Instant::now();
    for i in 0..1000 {
        s.write_at(
            Level::Debug,
            &format!("{i}:{}", "x".repeat(900)),
            Value::Null,
            at(b, (i / 200 + 1) * 1000),
        )
        .unwrap();
    }
    let r = s
        .read(ReadOptions {
            limit: 10,
            level: None,
        })
        .unwrap();
    assert_eq!(r.records.len(), 10);
    assert!(r.records[0].message.starts_with("990:"));
    assert!(r.records[9].message.starts_with("999:"));
}
#[test]
fn reply_size_is_bounded_and_keeps_newest_records() {
    let (_d, s) = store();
    let b = Instant::now();
    for i in 0..100 {
        s.write_at(
            Level::Info,
            &format!("{i}:{}", "x".repeat(8 * 1024)),
            Value::Null,
            at(b, (i / 200 + 1) * 1000),
        )
        .unwrap();
    }
    let r = s
        .read(ReadOptions {
            limit: 1000,
            level: None,
        })
        .unwrap();
    assert!(serde_json::to_vec(&r.records).unwrap().len() <= 192 * 1024);
    assert_eq!(
        r.records.last().unwrap().message.split(':').next(),
        Some("99")
    );
}
#[test]
fn rate_limit_drops_are_carried_once_by_next_successful_write() {
    let (_d, s) = store();
    let b = Instant::now();
    for _ in 0..200 {
        assert!(s.write_at(Level::Info, "ok", Value::Null, b).unwrap());
    }
    for _ in 0..3 {
        assert!(!s.write_at(Level::Info, "drop", Value::Null, b).unwrap());
    }
    assert!(!s
        .write_at(Level::Info, "drop", Value::Null, at(b, 999))
        .unwrap());
    assert!(s
        .write_at(Level::Info, "next", Value::Null, at(b, 1000))
        .unwrap());
    assert_eq!(
        s.read(ReadOptions {
            limit: 1000,
            level: None
        })
        .unwrap()
        .records
        .last()
        .unwrap()
        .dropped,
        Some(4)
    );
    s.write_at(Level::Info, "after", Value::Null, at(b, 1000))
        .unwrap();
    assert_eq!(
        s.read(ReadOptions {
            limit: 1000,
            level: None
        })
        .unwrap()
        .records
        .last()
        .unwrap()
        .dropped,
        None
    );
}
#[test]
fn failed_write_retains_dropped_count_for_next_success() {
    let (_d, s) = store();
    let b = Instant::now();
    for _ in 0..200 {
        assert!(s.write_at(Level::Info, "ok", Value::Null, b).unwrap());
    }
    for _ in 0..3 {
        assert!(!s.write_at(Level::Info, "drop", Value::Null, b).unwrap());
    }
    let logdir = s.log_dir();
    fs::remove_file(logdir.join("app.log")).unwrap();
    fs::create_dir(logdir.join("app.log")).unwrap();
    assert!(s
        .write_at(Level::Info, "fail", Value::Null, at(b, 1000))
        .is_err());
    fs::remove_dir(logdir.join("app.log")).unwrap();
    assert!(s
        .write_at(Level::Info, "next", Value::Null, at(b, 1000))
        .unwrap());
    assert_eq!(
        s.read(ReadOptions {
            limit: 1000,
            level: None
        })
        .unwrap()
        .records
        .last()
        .unwrap()
        .dropped,
        Some(3)
    );
}
#[test]
fn message_and_field_limits_preserve_utf8_and_mark_truncation() {
    let (_d, s) = store();
    let fields = json!({"large":"x".repeat(9000)});
    s.write(Level::Info, &format!("{}é", "x".repeat(8191)), fields)
        .unwrap();
    let r = s
        .read(ReadOptions {
            limit: 1,
            level: None,
        })
        .unwrap()
        .records
        .remove(0);
    assert!(r.message.len() <= 8192);
    assert!(r.message.ends_with("...[truncated]"));
    assert_eq!(
        Value::Object(r.fields.clone()),
        json!({"_dropped":"fields dropped: serialized fields exceeded 8192 bytes"})
    );
    assert!(serde_json::to_vec(&r.fields).unwrap().len() <= 8192);

    let boundary_fields = json!({"payload":""});
    let overhead = serde_json::to_vec(&boundary_fields).unwrap().len();
    let boundary_fields = json!({"payload":"x".repeat(8192 - overhead)});
    assert_eq!(serde_json::to_vec(&boundary_fields).unwrap().len(), 8192);
    s.write(Level::Info, "boundary", boundary_fields.clone())
        .unwrap();
    let written = s
        .read(ReadOptions {
            limit: 1,
            level: None,
        })
        .unwrap()
        .records
        .remove(0);
    assert_eq!(Value::Object(written.fields), boundary_fields);
}
#[test]
fn sensitive_keys_are_recursively_redacted_and_ordinary_values_remain() {
    let (_d, s) = store();
    s.write(Level::Info,"x",json!({"Password":"a","PaSsWd":"b","secretary":"c","access_token":"d","authorization":"e","cookieJar":"f","my_api_key":"g","nested":[{"Api-Key":"h"}],"ordinary":"visible"})).unwrap();
    let r = s
        .read(ReadOptions {
            limit: 1,
            level: None,
        })
        .unwrap()
        .records
        .remove(0);
    for k in [
        "Password",
        "PaSsWd",
        "secretary",
        "access_token",
        "authorization",
        "cookieJar",
        "my_api_key",
    ] {
        assert_eq!(r.fields[k], "[redacted]");
    }
    assert_eq!(r.fields["nested"][0]["Api-Key"], "[redacted]");
    assert_eq!(r.fields["ordinary"], "visible");
}
#[test]
fn serialized_record_is_rfc3339_jsonl_with_control_characters_escaped() {
    let (_d, s) = store();
    s.write(Level::Warn, "line\n\t\r", json!({"ok":true}))
        .unwrap();
    let raw = fs::read_to_string(s.log_dir().join("app.log")).unwrap();
    assert_eq!(raw.lines().count(), 1);
    let v: serde_json::Value = serde_json::from_str(raw.trim()).unwrap();
    let r: super::LogRecord = serde_json::from_value(v.clone()).unwrap();
    assert!(
        valid_rfc3339_millis(&r.time),
        "invalid timestamp: {}",
        r.time
    );
    for invalid in [
        "2026-01-01T10:11:12.xxxZ",
        "junkT10:11:12.000Z",
        "2026-00-01T10:11:12.000Z",
        "2026-13-01T10:11:12.000Z",
        "2026-01-00T10:11:12.000Z",
        "2026-01-32T10:11:12.000Z",
        "2026-01-01T24:11:12.000Z",
        "2026-01-01T10:60:12.000Z",
        "2026-01-01T10:11:60.000Z",
        "2026-01-01T10:11:12.000ZjunkZ",
    ] {
        assert!(!valid_rfc3339_millis(invalid));
    }
    assert!(valid_rfc3339_millis("2026-01-01T10:11:12Z"));
    assert_eq!(v["message"], "line\n\t\r");
}
#[test]
fn crash_reports_hide_home_environment_and_include_runtime_details() {
    let mut runtime = RuntimeInfo {
        home: Some("HOME_SECRET".into()),
        ..Default::default()
    };
    runtime.app_id = "app-value".into();
    runtime.version = "version-value".into();
    runtime.runtime = "runtime-value".into();
    runtime.os = "os-value".into();
    runtime.arch = "arch-value".into();
    let r = CrashReport {
        id: "x".into(),
        message: "panic HOME_SECRET".into(),
        location: Some("HOME_SECRET/src".into()),
        backtrace: "trace HOME_SECRET".into(),
        runtime,
    };
    let text = r.build_with_environment(&[(
        "SENTINEL_ENV_KEY".into(),
        "unique-environment-secret-73A9".into(),
    )]);
    assert!(text.contains("~/src"));
    assert!(!text.contains("HOME_SECRET"));
    assert!(!text.to_lowercase().contains("environment"));
    for value in [
        "app-value",
        "version-value",
        "runtime-value",
        "os-value",
        "arch-value",
        "backtrace",
        "time",
    ] {
        assert!(text.contains(value), "missing {value}");
    }
    assert!(!text.contains("unique-environment-secret-73A9"));
}
#[test]
fn crash_reports_retain_twenty_bounded_reports() {
    let (d, s) = store();
    let info: CrashInfo = s.save_crash_auto(&"x".repeat(70 * 1024)).unwrap();
    let text = s.crash(&info.id).unwrap();
    assert_eq!(text.len(), 64 * 1024);
    assert!(text.ends_with("...[truncated]"));
    assert!(info.time.contains('T'));
    assert_eq!(info.size, 64 * 1024);
    let retention = LogStore::new(d.path().join("retention")).unwrap();
    for i in 0..21 {
        retention.save_crash(&format!("{i:020}-test"), "x").unwrap();
    }
    let list = retention.crashes().unwrap();
    assert_eq!(list.len(), 20);
    assert!(!list.iter().any(|x| x.id == "00000000000000000000-test"));
    for i in 1..=20 {
        assert!(list.iter().any(|x| x.id == format!("{i:020}-test")));
    }
}
#[test]
fn crash_ids_reject_traversal_and_missing_ids_error() {
    let (_d, s) = store();
    for x in ["", "..", "../x", "/x", "\\x", "C:x", "a:b", "a\\b", "a/b"] {
        assert_eq!(s.crash(x).unwrap_err().code, ErrorCode::InvalidArgument);
        assert!(s.save_crash(x, "x").is_err());
    }
    assert!(s.crash("missing").is_err());
}
#[test]
fn huge_corrupt_files_are_scanned_with_bounded_bytes_and_line_lengths() {
    let (_d, s) = store();
    let dir = s.prepare(false).unwrap();
    fs::write(dir.join("app.log"), vec![b'x'; 8 * 1024 * 1024]).unwrap();
    let r = s
        .read(ReadOptions {
            limit: 100,
            level: None,
        })
        .unwrap();
    assert!(r.records.is_empty());
    assert!(r.dropped_bytes <= 5 * 1024 * 1024);
}
#[test]
fn distinct_apps_are_isolated_and_independent_same_root_writers_are_atomic() {
    let (d, a) = store();
    let other = LogStore::new(d.path().join("other")).unwrap();
    let (tx, rx) = mpsc::channel();
    let mut hs = Vec::new();
    for n in 0..4 {
        let root = d.path().to_path_buf();
        let tx = tx.clone();
        hs.push(thread::spawn(move || {
            let s = LogStore::new(root).unwrap();
            for i in 0..25 {
                s.write(Level::Debug, &format!("{n}:{i}"), Value::Null)
                    .unwrap();
            }
            let _ = tx.send(());
        }));
    }
    drop(tx);
    for _ in 0..4 {
        rx.recv_timeout(Duration::from_secs(5)).unwrap();
    }
    for h in hs {
        h.join().unwrap();
    }
    assert_eq!(
        a.read(ReadOptions {
            limit: 1000,
            level: None
        })
        .unwrap()
        .records
        .len(),
        100
    );
    assert!(other
        .read(ReadOptions {
            limit: 100,
            level: None
        })
        .unwrap()
        .records
        .is_empty());
    for line in fs::read_to_string(a.log_dir().join("app.log"))
        .unwrap()
        .lines()
    {
        let _: super::LogRecord = serde_json::from_str(line).unwrap();
    }
}
#[test]
fn reads_reject_overlarge_limits_and_apply_exact_level_filters() {
    let (_d, s) = store();
    s.write(Level::Info, "i", Value::Null).unwrap();
    s.write(Level::Error, "e", Value::Null).unwrap();
    assert!(s
        .read(ReadOptions {
            limit: 0,
            level: None
        })
        .unwrap()
        .records
        .is_empty());
    assert_eq!(
        s.read(ReadOptions {
            limit: 1001,
            level: None
        })
        .unwrap_err()
        .code,
        ErrorCode::InvalidArgument
    );
    assert_eq!(
        s.read(ReadOptions {
            limit: 10,
            level: Some(Level::Error)
        })
        .unwrap()
        .records
        .len(),
        1
    );
}
#[cfg(unix)]
#[test]
fn symlinked_storage_paths_are_rejected_and_created_modes_are_private() {
    use std::os::unix::fs::{symlink, PermissionsExt};
    let d = tempfile::tempdir().unwrap();
    let s = LogStore::new(d.path()).unwrap();
    s.write(Level::Info, "ok", Value::Null).unwrap();
    let logdir = s.log_dir();
    assert_eq!(
        fs::metadata(&logdir).unwrap().permissions().mode() & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(logdir.join("app.log"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
    let target = d.path().join("target");
    fs::create_dir(&target).unwrap();
    let root2 = d.path().join("symlinks");
    fs::create_dir(&root2).unwrap();
    symlink(&target, root2.join("logs")).unwrap();
    assert!(LogStore::new(&root2)
        .unwrap()
        .read(ReadOptions {
            limit: 1,
            level: None
        })
        .is_err());
    let root3 = d.path().join("crash-link");
    fs::create_dir(&root3).unwrap();
    symlink(&target, root3.join("crashes")).unwrap();
    assert!(LogStore::new(&root3)
        .unwrap()
        .save_crash("00000000000000000001-test", "x")
        .is_err());
    let crashroot = d.path().join("private-crash");
    let cs = LogStore::new(&crashroot).unwrap();
    cs.save_crash("00000000000000000002-test", "x").unwrap();
    assert_eq!(
        fs::metadata(crashroot.join("crashes"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o700
    );
    assert_eq!(
        fs::metadata(crashroot.join("crashes/00000000000000000002-test.txt"))
            .unwrap()
            .permissions()
            .mode()
            & 0o777,
        0o600
    );
}

#[test]
fn emergency_write_does_not_wait_for_a_held_crash_lock() {
    let (directory, store) = store();
    store
        .save_crash("00000000000000000001-seed", "seed")
        .unwrap();
    let folder = directory.path().join("crashes");
    let lock = fs::OpenOptions::new()
        .read(true)
        .write(true)
        .open(folder.join(".lock"))
        .unwrap();
    lock.lock().unwrap();
    let worker_folder = folder.clone();
    let (sender, receiver) = mpsc::channel();
    let worker = thread::spawn(move || {
        let _ = sender.send(super::storage::emergency_save(&worker_folder, "blocked").is_err());
    });
    let completed_without_waiting = receiver.recv_timeout(Duration::from_secs(5)).ok();
    drop(lock);
    worker.join().unwrap();
    assert_eq!(completed_without_waiting, Some(true));
    assert_eq!(store.crashes().unwrap().len(), 1);
}

static PANIC_HOOK_TEST: std::sync::Mutex<()> = std::sync::Mutex::new(()); // Serialize tests mutating the global panic hook.

#[test]
fn installed_panic_hook_writes_before_previous_and_retains_twenty_reports() {
    use std::{
        panic::PanicHookInfo,
        sync::atomic::{AtomicBool, AtomicUsize, Ordering},
    };
    type Hook = Box<dyn for<'a> Fn(&PanicHookInfo<'a>) + Send + Sync + 'static>;
    struct RestoreHook(Option<Hook>);
    impl Drop for RestoreHook {
        fn drop(&mut self) {
            if let Some(hook) = self.0.take() {
                drop(std::panic::take_hook());
                std::panic::set_hook(hook);
            }
        }
    }
    // Serialize installation/restoration because panic hooks are process-global; filtering below keeps unrelated parallel test panics on the original hook.
    let _lock = PANIC_HOOK_TEST.lock().unwrap_or_else(|e| e.into_inner());
    let (directory, store) = store();
    for i in 0..20 {
        store.save_crash(&format!("{i:020}-seed"), "seed").unwrap();
    }
    let original: std::sync::Arc<Hook> = std::sync::Arc::new(std::panic::take_hook());
    let restore_original = original.clone();
    let restore = RestoreHook(Some(Box::new(move |info| restore_original(info))));
    let called = std::sync::Arc::new(AtomicUsize::new(0));
    let observed = std::sync::Arc::new(AtomicBool::new(false));
    let folder = directory.path().join("crashes");
    let hook_folder = folder.clone();
    let hook_called = called.clone();
    let hook_observed = observed.clone();
    let previous: Hook = Box::new(move |_| {
        let reports: Vec<_> = fs::read_dir(&hook_folder)
            .map(|entries| {
                entries
                    .filter_map(Result::ok)
                    .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "txt"))
                    .collect()
            })
            .unwrap_or_default();
        let new = reports
            .iter()
            .find(|e| !e.file_name().to_string_lossy().contains("-seed"));
        let valid = reports.len() == 20
            && new.is_some_and(|entry| {
                let text = fs::read_to_string(entry.path()).unwrap_or_default();
                text.contains("panic-before-previous ~")
                    && text.contains("app: hook-order-test")
                    && text.contains("version: 1.2.3")
                    && text.contains("runtime: test-runtime")
                    && text.contains(&format!("os: {}", std::env::consts::OS))
                    && text.contains(&format!("arch: {}", std::env::consts::ARCH))
                    && !text.contains("HOME_SECRET")
                    && text.len() <= 65_536
                    && (!text.contains("...[truncated]") || text.ends_with("...[truncated]"))
            });
        hook_observed.store(valid, Ordering::SeqCst);
        hook_called.fetch_add(1, Ordering::SeqCst);
    });
    let metadata = RuntimeInfo {
        app_id: "hook-order-test".into(),
        version: "1.2.3".into(),
        runtime: "test-runtime".into(),
        os: std::env::consts::OS.into(),
        arch: std::env::consts::ARCH.into(),
        home: Some("HOME_SECRET".into()),
    };
    let custom_hook = super::crash::make_panic_hook(folder, metadata, previous);
    let installed_original = original.clone();
    std::panic::set_hook(Box::new(move |info| {
        let payload = info.payload();
        let message = payload
            .downcast_ref::<String>()
            .map(String::as_str)
            .or_else(|| payload.downcast_ref::<&str>().copied());
        if message.is_some_and(|message| message.contains("panic-before-previous HOME_SECRET")) {
            custom_hook(info);
        } else {
            installed_original(info);
        }
    }));
    let (sender, receiver) = mpsc::channel();
    let panicker = thread::spawn(move || {
        let result = std::panic::catch_unwind(|| {
            panic!(
                "panic-before-previous HOME_SECRET {}",
                "x".repeat(70 * 1024)
            )
        });
        let _ = sender.send(result.is_err());
    });
    let received = receiver.recv_timeout(Duration::from_secs(5));
    let joined = if received.is_ok() {
        Some(panicker.join())
    } else {
        None
    };
    drop(restore);
    assert!(received.unwrap());
    joined.unwrap().unwrap();
    assert_eq!(called.load(Ordering::SeqCst), 1);
    assert!(observed.load(Ordering::SeqCst));
    let reports = store.crashes().unwrap();
    assert_eq!(reports.len(), 20);
    assert!(!reports
        .iter()
        .any(|r| r.id.starts_with("00000000000000000000") && r.id.ends_with("-seed")));
    let new = reports.iter().find(|r| !r.id.ends_with("-seed")).unwrap();
    let text = store.crash(&new.id).unwrap();
    assert!(text.len() <= 65_536);
    assert!(text.ends_with("...[truncated]"));
}
