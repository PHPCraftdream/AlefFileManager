// SPDX-License-Identifier: MIT OR Apache-2.0
#![cfg(target_os = "macos")]
use super::*;
use std::{
    path::PathBuf,
    sync::atomic::{AtomicU64, Ordering},
};
static NEXT: AtomicU64 = AtomicU64::new(0);
struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        loop {
            let p = std::env::temp_dir().join(format!(
                "alef-native-mac-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match std::fs::create_dir(&p) {
                Ok(()) => return Self(std::fs::canonicalize(p).unwrap()),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => panic!("{e}"),
            }
        }
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}
fn artifact(root: &Path, name: &str, version: &str) -> Artifact {
    let package = root.join(format!("{version}.alef"));
    std::fs::write(&package, b"verified fixture bytes").unwrap();
    let icon = root.join("icon.png");
    std::fs::write(&icon, b"fixture icon").unwrap();
    Artifact {
        id: "org.example.native-test".into(),
        name: name.into(),
        hash: sha256_id("org.example.native-test"),
        executable: std::env::current_exe().unwrap(),
        package,
        icon: Some(icon),
    }
}
fn set_root(path: &Path) {
    std::env::set_var("ALEF_APPLICATIONS_DIR", path);
}
#[test]
fn mac_install_replaces_bundle_and_rollback_republishes_previous_launcher() {
    let scratch = Scratch::new();
    let root = scratch.0.join("Applications");
    std::fs::create_dir(&root).unwrap();
    let old = artifact(&scratch.0, "Old Name", "1");
    let next = artifact(&scratch.0, "New Name", "2");
    mac_install(&root, &safe_display_component(&old.name, &old.hash), &old).unwrap();
    mac_install(
        &root,
        &safe_display_component(&next.name, &next.hash),
        &next,
    )
    .unwrap();
    let new_bundle = root.join(format!(
        "{}.app",
        safe_display_component(&next.name, &next.hash)
    ));
    let old_bundle = root.join(format!(
        "{}.app",
        safe_display_component(&old.name, &old.hash)
    ));
    assert!(!old_bundle.exists());
    assert!(new_bundle.exists());
    let contents = new_bundle.join("Contents");
    let plist = std::fs::read_to_string(contents.join("Info.plist")).unwrap();
    assert!(plist.contains("New Name"));
    let launcher = contents.join("MacOS/launcher");
    let text = std::fs::read_to_string(&launcher).unwrap();
    assert!(text.contains(&next.package.to_string_lossy().to_string()));
    use std::os::unix::fs::PermissionsExt;
    assert_eq!(
        std::fs::metadata(&launcher).unwrap().permissions().mode() & 0o777,
        0o755
    );
    // A later record-publication failure is modeled by restoring the old artifact.
    mac_install(&root, &safe_display_component(&old.name, &old.hash), &old).unwrap();
    assert!(!new_bundle.exists());
    assert!(old_bundle.exists());
    assert!(
        std::fs::read_to_string(old_bundle.join("Contents/MacOS/launcher"))
            .unwrap()
            .contains(&old.package.to_string_lossy().to_string())
    );
}
#[cfg(unix)]
#[test]
fn malformed_stale_bundle_is_rejected_without_replacing_existing_bundle() {
    use std::os::unix::fs::symlink;
    let scratch = Scratch::new();
    let root = scratch.0.join("Applications");
    std::fs::create_dir(&root).unwrap();
    let old = artifact(&scratch.0, "Old Name", "1");
    let next = artifact(&scratch.0, "New Name", "2");
    mac_install(&root, &safe_display_component(&old.name, &old.hash), &old).unwrap();
    let stale = root.join(format!("Stale~{}.app", old.hash));
    std::fs::create_dir_all(stale.join("Contents")).unwrap();
    symlink(&scratch.0, stale.join("Contents/linked")).unwrap();
    assert!(mac_install(
        &root,
        &safe_display_component(&next.name, &next.hash),
        &next
    )
    .is_err());
    assert!(root
        .join(format!(
            "{}.app",
            safe_display_component(&old.name, &old.hash)
        ))
        .exists());

    assert!(std::fs::read_dir(&root).unwrap().all(|e| !e
        .unwrap()
        .file_name()
        .to_string_lossy()
        .starts_with(".alef-stage-")));
}
