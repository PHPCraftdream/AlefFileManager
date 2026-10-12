// SPDX-License-Identifier: MIT OR Apache-2.0
use super::*;
use alef_core::security::{
    manifest::Permissions,
    permissions::{Grants, PathVars, Permission, PermissionSet},
};

struct Swap {
    _temp: tempfile::TempDir,
    inside: PathBuf,
    outside: PathBuf,
}

impl Swap {
    fn new() -> Self {
        let temp = tempfile::tempdir().unwrap();
        let inside = temp.path().join("allowed");
        let outside = temp.path().join("outside");
        fs::create_dir(&inside).unwrap();
        fs::create_dir(&outside).unwrap();
        fs::write(inside.join("file"), b"inside").unwrap();
        fs::write(outside.join("file"), b"outside secret").unwrap();
        Self {
            _temp: temp,
            inside,
            outside,
        }
    }

    fn authorized(&self, name: &str, write: bool) -> PathBuf {
        let root = self._temp.path().to_path_buf();
        let vars = PathVars {
            app_data: root.clone(),
            app_config: root.clone(),
            app_cache: root.clone(),
            home: root.clone(),
            documents: root.clone(),
            downloads: root.clone(),
            desktop: root.clone(),
            temp: root.clone(),
            app: Some(root),
        };
        let policy: Permissions = serde_json::from_value(serde_json::json!({
            "fs": {"read": [], "write": []}, "cli": {"exec": []},
            "net": {"http": [], "socket": []}, "shell": {"openExternal": []},
            "clipboard": {"read": false}, "shortcut": {"global": false},
            "secrets": false, "app": {"env": []}
        }))
        .unwrap();
        let permissions = PermissionSet::from_manifest(&policy, &vars).unwrap();
        let grants = Grants::default();
        grants.grant_write(&self.inside).unwrap();
        grants.grant_read(&self.inside).unwrap();
        permissions
            .authorize_path(
                if write {
                    Permission::FsWrite
                } else {
                    Permission::FsRead
                },
                Some(self.inside.join(name).to_str().unwrap()),
                &grants,
            )
            .unwrap()
    }

    fn swap(&self) {
        fs::rename(&self.inside, self._temp.path().join("original")).unwrap();
        #[cfg(unix)]
        std::os::unix::fs::symlink(&self.outside, &self.inside).unwrap();
        #[cfg(windows)]
        {
            use std::{
                process::{Command, Stdio},
                time::{Duration, Instant},
            };
            let mut child = Command::new("cmd.exe")
                .args(["/D", "/C", "mklink", "/J"])
                .arg(&self.inside)
                .arg(&self.outside)
                .stdout(Stdio::null())
                .stderr(Stdio::null())
                .spawn()
                .unwrap();
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                if let Some(status) = child.try_wait().unwrap() {
                    assert!(status.success(), "mklink /J failed");
                    break;
                }
                if Instant::now() >= deadline {
                    child.kill().unwrap();
                    child.wait().unwrap();
                    panic!("mklink /J timed out");
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
    }

    fn open(&self, name: &str, options: &OpenOptions, write: bool) -> Result<File, AlefError> {
        let authorized = self.authorized(name, write);
        checked_open(&authorized, options, |options, path| {
            self.swap();
            options.open(path)
        })
    }
}

impl Drop for Swap {
    fn drop(&mut self) {
        // Unlink the junction before TempDir recursively cleans up, also on assertion failure.
        #[cfg(windows)]
        let _ = fs::remove_dir(&self.inside);
        #[cfg(unix)]
        let _ = fs::remove_file(&self.inside);
    }
}

#[test]
fn read_file_denies_frozen_place_after_swap() {
    let swap = Swap::new();
    let real = swap.authorized("file", false);
    let place = Place::for_test(real.clone(), real, None);
    swap.swap();
    assert_eq!(
        read_file(&place, 1024).unwrap_err().code,
        ErrorCode::PermissionDenied
    );
}

#[test]
fn write_file_denies_frozen_place_after_swap() {
    for append in [false, true] {
        let swap = Swap::new();
        let real = swap.authorized("file", true);
        let place = Place::for_test(real.clone(), real, None);
        swap.swap();
        let result = write_file(&place, b"attack", append, false);
        assert_eq!(
            fs::read(swap.outside.join("file")).unwrap(),
            b"outside secret"
        );
        assert_eq!(result.unwrap_err().code, ErrorCode::PermissionDenied);
    }
}

#[test]
fn open_for_handle_denies_frozen_place_after_swap() {
    for truncate in [false, true] {
        let swap = Swap::new();
        let real = swap.authorized("file", true);
        let place = Place::for_test(real.clone(), real, None);
        swap.swap();
        let mut options = OpenOptions::new();
        options.read(true).write(true);
        let result = open_for_handle(&place, &options, truncate);
        assert_eq!(
            fs::read(swap.outside.join("file")).unwrap(),
            b"outside secret"
        );
        assert_eq!(result.unwrap_err().code, ErrorCode::PermissionDenied);
    }
}

#[test]
fn open_for_handle_makes_the_file_empty_only_when_asked() {
    for truncate in [false, true] {
        let swap = Swap::new();
        let real = swap.authorized("file", true);
        let place = Place::for_test(real.clone(), real.clone(), None);
        let mut options = OpenOptions::new();
        options.read(true).write(true);
        open_for_handle(&place, &options, truncate).unwrap();
        assert_eq!(fs::read(&real).unwrap().is_empty(), truncate);
    }
}

#[test]
fn a_handle_whose_path_cannot_be_read_is_refused() {
    let swap = Swap::new();
    let real = swap.authorized("file", false);
    let refused = checked_open_by(
        &real,
        OpenOptions::new().read(true),
        |options, path| options.open(path),
        |_| Err(std::io::Error::other("the system cannot tell")),
    );
    assert_eq!(refused.unwrap_err().code, ErrorCode::PermissionDenied);
}

#[test]
fn the_spelling_of_a_path_drops_current_and_parent_components() {
    assert_eq!(
        comparable_path(Path::new("x/./y/../z")),
        PathBuf::from("x/z")
    );
}

#[test]
fn create_file_denies_frozen_place_after_swap() {
    let swap = Swap::new();
    let real = swap.authorized("file", true);
    let place = Place::for_test(real.clone(), real, None);
    swap.swap();
    let result = create_file(&place);
    assert_eq!(
        fs::read(swap.outside.join("file")).unwrap(),
        b"outside secret"
    );
    assert_eq!(result.unwrap_err().code, ErrorCode::PermissionDenied);
}

#[test]
fn write_file_overwrite_removes_old_tail() {
    let swap = Swap::new();
    let real = swap.authorized("file", true);
    let place = Place::for_test(real.clone(), real, None);
    write_file(&place, b"x", false, false).unwrap();
    assert_eq!(fs::read(&place.real).unwrap(), b"x");
}

#[test]
fn checked_open_accepts_stand_in_spelling() {
    let swap = Swap::new();
    let canonical = fs::canonicalize(&swap.inside).unwrap();
    let root = canonical.join(".").join("scope");
    let real = root.join("file");
    let place = Place::for_test(real.clone(), real.clone(), Some(root));
    write_file(&place, b"normal", false, true).unwrap();
    assert_eq!(read_file(&place, 1024).unwrap(), b"normal");
    write_file(&place, b"+append", true, false).unwrap();
    assert_eq!(read_file(&place, 1024).unwrap(), b"normal+append");
    let file = create_file(&place).unwrap();
    assert_eq!(file.metadata().unwrap().len(), 0);
    #[cfg(windows)]
    {
        let ordinary = comparable_path(&real);
        let upper = PathBuf::from(ordinary.to_str().unwrap().to_ascii_uppercase());
        checked_open(&upper, OpenOptions::new().read(true), |options, path| {
            options.open(path)
        })
        .unwrap();
        assert_eq!(
            comparable_path(Path::new(r"\\?\UNC\Server\Share\file")),
            comparable_path(Path::new(r"\\server\share\FILE"))
        );
    }
}

#[test]
fn checked_open_denies_swapped_read() {
    let swap = Swap::new();
    let result = swap.open("file", OpenOptions::new().read(true), false);
    assert_eq!(result.unwrap_err().code, ErrorCode::PermissionDenied);
}

#[test]
fn checked_open_denies_swapped_overwrite_and_truncate() {
    let swap = Swap::new();
    let result = swap
        .open("file", OpenOptions::new().write(true).truncate(true), true)
        .and_then(|mut file| {
            file.set_len(0).map_err(fault)?;
            file.write_all(b"attack").map_err(fault)
        });
    assert_eq!(
        fs::read(swap.outside.join("file")).unwrap(),
        b"outside secret"
    );
    assert_eq!(result.unwrap_err().code, ErrorCode::PermissionDenied);
}

#[test]
fn checked_open_denies_swapped_append() {
    let swap = Swap::new();
    let result = swap
        .open("file", OpenOptions::new().append(true), true)
        .and_then(|mut file| file.write_all(b"attack").map_err(fault));
    assert_eq!(
        fs::read(swap.outside.join("file")).unwrap(),
        b"outside secret"
    );
    assert_eq!(result.unwrap_err().code, ErrorCode::PermissionDenied);
}

#[test]
fn checked_open_denies_swapped_create_and_create_new() {
    for exclusive in [false, true] {
        let swap = Swap::new();
        let mut options = OpenOptions::new();
        options
            .read(true)
            .write(true)
            .create(!exclusive)
            .create_new(exclusive);
        let result = swap
            .open("new", &options, true)
            .and_then(|mut file| file.write_all(b"attack").map_err(fault));
        assert_eq!(fs::read(swap.outside.join("new")).unwrap(), b"");
        assert_eq!(result.unwrap_err().code, ErrorCode::PermissionDenied);
    }
}
