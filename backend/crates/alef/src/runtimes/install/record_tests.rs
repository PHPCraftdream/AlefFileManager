// SPDX-License-Identifier: MIT OR Apache-2.0
//! Scratch tests for strict installed-application records.
#[cfg(test)]
mod tests {
    use super::super::{
        list_records, read_record, valid_id, AppListing, AppRecord, InstallError, RECORD_LIMIT,
    };
    use sha2::Digest;
    use std::{
        path::{Path, PathBuf},
        sync::atomic::{AtomicU64, Ordering},
    };

    static NEXT: AtomicU64 = AtomicU64::new(0);
    struct Scratch(PathBuf);
    impl Scratch {
        fn new() -> Self {
            let path = loop {
                let path = std::env::temp_dir().join(format!(
                    "alef-record-{}-{}",
                    std::process::id(),
                    NEXT.fetch_add(1, Ordering::Relaxed)
                ));
                match std::fs::create_dir(&path) {
                    Ok(()) => break std::fs::canonicalize(&path).expect("canonical scratch"),
                    Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                    Err(error) => panic!("scratch: {error}"),
                }
            };
            Self(path)
        }
    }
    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
    fn record(id: &str, version: &str) -> AppRecord {
        AppRecord {
            id: id.into(),
            name: "Name".into(),
            version: version.into(),
            icon: None,
            package: format!("{version}.alef"),
            executable: std::env::current_exe().unwrap(),
            installed_time: 1,
        }
    }
    fn put(root: &Path, id: &str, version: &str) -> String {
        let hash = format!("{:x}", sha2::Sha256::digest(id.as_bytes()));
        let dir = root.join(&hash);
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(
            dir.join("app.json"),
            serde_json::to_vec(&record(id, version)).unwrap(),
        )
        .unwrap();
        std::fs::write(dir.join(format!("{version}.alef")), b"package").unwrap();
        hash
    }
    #[test]
    fn strict_semantics_accept_semver_metadata_and_reject_bad_records() {
        let good = record("org.example.app", "1.2.3-rc.1+build.7");
        assert!(alef_core::security::manifest::validate_package_version(&good.version).is_ok());
        for id in ["", ".a", "a.", "a..b", "a/b", "é"] {
            assert!(!valid_id(id), "{id:?}");
        }
        assert!(valid_id("org.example-app_2"));
        let scratch = Scratch::new();
        let root = &scratch.0;
        let hash = format!("{:x}", sha2::Sha256::digest(good.id.as_bytes()));
        let dir = root.join(&hash);
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("app.json"), serde_json::to_vec(&good).unwrap()).unwrap();
        std::fs::write(dir.join(&good.package), b"package").unwrap();
        assert_eq!(read_record(&dir, &hash).unwrap(), good);
        let original = serde_json::to_value(&good).unwrap();
        let mut unknown = original.as_object().unwrap().clone();
        unknown.insert("unknown".into(), serde_json::json!(1));
        let unknown = serde_json::Value::Object(unknown);
        std::fs::write(dir.join("app.json"), serde_json::to_vec(&unknown).unwrap()).unwrap();
        assert_eq!(read_record(&dir, &hash).unwrap_err().category, "APP_RECORD");
        let serialized = serde_json::to_string(&good).unwrap();
        let id_value = serde_json::to_value(&good.id).unwrap();
        let id_field = format!("\"id\":{id_value}");
        assert!(serialized.contains(&id_field));
        let duplicate_id = format!("\"id\":{id_value},\"id\":{id_value}");
        let duplicate = serialized.replacen(&id_field, &duplicate_id, 1);
        std::fs::write(dir.join("app.json"), duplicate).unwrap();
        assert_eq!(read_record(&dir, &hash).unwrap_err().category, "APP_RECORD");
        std::fs::write(dir.join("app.json"), vec![b' '; RECORD_LIMIT as usize + 1]).unwrap();
        assert!(read_record(&dir, &hash)
            .unwrap_err()
            .detail
            .contains("size limit"));
        std::fs::write(
            dir.join("app.json"),
            serde_json::to_vec(&record("wrong.id", "1.0.0")).unwrap(),
        )
        .unwrap();
        assert_eq!(read_record(&dir, &hash).unwrap_err().category, "APP_RECORD");
        assert_eq!(
            read_record(&dir, &"a".repeat(64)).unwrap_err().category,
            "APP_RECORD"
        );
        std::fs::write(
            dir.join("app.json"),
            serde_json::to_vec(&record("org.example.app", "01.0.0")).unwrap(),
        )
        .unwrap();
        assert_eq!(read_record(&dir, &hash).unwrap_err().category, "APP_RECORD");
    }
    #[test]
    fn listing_preserves_production_order_and_damage() {
        let scratch = Scratch::new();
        let root = scratch.0.join("missing");
        assert!(list_records(&root).unwrap().is_empty());
        std::fs::create_dir(&root).unwrap();
        let damaged_id = "org.example.b";
        let damaged_hash = format!("{:x}", sha2::Sha256::digest(damaged_id.as_bytes()));
        let damaged = root.join(&damaged_hash);
        std::fs::create_dir(&damaged).unwrap();
        std::fs::write(damaged.join("app.json"), b"broken").unwrap();
        std::fs::write(root.join(".ignored"), b"ignore").unwrap();
        put(&root, "org.example.z", "1.0.0");
        put(&root, "org.example.a", "1.0.0");
        put(&root, "org.example.m", "1.0.0");
        let rows = list_records(&root).unwrap();
        let installed = rows
            .iter()
            .filter_map(|row| match row {
                AppListing::Installed(record) => Some(record.id.as_str()),
                AppListing::Damaged { .. } => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(installed.len(), 3);
        assert_eq!(
            installed,
            ["org.example.a", "org.example.m", "org.example.z"]
        );
        assert_eq!(rows.len(), 4);
        assert!(rows
            .iter()
            .any(|row| matches!(row, AppListing::Damaged { hash } if hash == &damaged_hash)));
    }
}
