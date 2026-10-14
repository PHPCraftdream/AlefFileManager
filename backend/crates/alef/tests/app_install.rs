#![cfg(feature = "e2e")]

#[allow(dead_code)]
#[path = "../src/application/support.rs"]
mod support;

use std::{
    fs,
    path::{Path, PathBuf},
    process::{Child, Command, Stdio},
    sync::atomic::{AtomicU64, Ordering},
    time::{Duration, Instant},
};

static NEXT: AtomicU64 = AtomicU64::new(0);

struct Scratch(PathBuf);

impl Scratch {
    fn new() -> Self {
        loop {
            let path = std::env::temp_dir().join(format!(
                "alef-app-install-{}-{}",
                std::process::id(),
                NEXT.fetch_add(1, Ordering::Relaxed)
            ));
            match fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(error) if error.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(error) => panic!("create scratch directory: {error}"),
            }
        }
    }

    fn child(&self, sequence: usize, args: &[&str]) -> (i32, String, String) {
        let stdout_path = self.0.join(format!("stdout-{sequence}.log"));
        let stderr_path = self.0.join(format!("stderr-{sequence}.log"));
        let stdout = fs::File::create(&stdout_path).expect("create stdout log");
        let stderr = fs::File::create(&stderr_path).expect("create stderr log");
        let home = absolute_dir(&self.0.join("home"));
        let applications = absolute_dir(&self.0.join("apps"));
        let app_home = absolute_dir(&self.0.join("state"));
        let mut command = Command::new(env!("CARGO_BIN_EXE_alef"));
        command
            .args(args)
            .env("ALEF_HOME", home)
            .env("ALEF_APPLICATIONS_DIR", applications)
            .env("ALEF_APP_HOME", app_home)
            .env_remove("ALEF_E2E_TRUST")
            .env_remove("ALEF_E2E")
            .stdin(Stdio::null())
            .stdout(Stdio::from(stdout))
            .stderr(Stdio::from(stderr));
        let mut child = command.spawn().expect("spawn alef command");
        let code = bounded_exit(&mut child, Duration::from_secs(120));
        let out = fs::read_to_string(stdout_path).expect("read stdout log");
        let err = fs::read_to_string(stderr_path).expect("read stderr log");
        (code, out, err)
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn absolute_dir(path: &Path) -> PathBuf {
    fs::create_dir_all(path).expect("create isolated directory");
    fs::canonicalize(path).expect("canonicalize isolated directory")
}

fn bounded_exit(child: &mut Child, timeout: Duration) -> i32 {
    let deadline = Instant::now() + timeout;
    loop {
        match child.try_wait() {
            Ok(Some(status)) => return status.code().unwrap_or(-1),
            Ok(None) if Instant::now() < deadline => std::thread::sleep(Duration::from_millis(20)),
            Ok(None) => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("alef child exceeded {timeout:?}");
            }
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                panic!("wait for alef child: {error}");
            }
        }
    }
}

#[test]
fn signed_package_install_is_listed_idempotent_and_uninstallable_without_touching_host_state() {
    use sha2::{Digest, Sha256};

    let scratch = Scratch::new();
    let id = format!(
        "org.alef.install.p{}c{}",
        std::process::id(),
        NEXT.fetch_add(1, Ordering::Relaxed)
    );
    let manifest = support::manifest_text("")
        .replace("id: org.example.app", &format!("id: {id}"))
        .replace("name: Example", "name: Install CI");
    let content = b"<!doctype html><title>install fixture</title>";
    let icon = include_bytes!("../../../../frontend/public/logo-32x32.png");
    let (package_bytes, fingerprint) = support::signed_package(&[
        ("alef.ktav", manifest.as_bytes()),
        ("dist/index.html", content),
        ("icon.png", icon),
    ]);
    let package = scratch.0.join("fixture.alef");
    fs::write(&package, &package_bytes).expect("write signed package fixture");
    let hash = format!("{:x}", Sha256::digest(id.as_bytes()));
    let home = absolute_dir(&scratch.0.join("home"));
    let applications = absolute_dir(&scratch.0.join("apps"));
    let app_home = absolute_dir(&scratch.0.join("state"));
    let app_dir = home.join("apps").join(&hash);

    let package_arg = package.display().to_string();
    let (code, out, err) =
        scratch.child(0, &["install", &package_arg, "--trust-key", &fingerprint]);
    assert_eq!(code, 0, "install failed; stdout={out:?}, stderr={err:?}");
    assert!(app_dir.join("app.json").is_file(), "app record missing");
    assert_eq!(
        fs::read(app_dir.join("1.0.0.alef")).expect("stored package"),
        package_bytes
    );
    assert_eq!(
        fs::read(app_dir.join("icon.png")).expect("installed app icon"),
        icon
    );

    let (code, out, err) = scratch.child(1, &["apps", "--json"]);
    assert_eq!(code, 0, "apps failed; stdout={out:?}, stderr={err:?}");
    let rows: serde_json::Value = serde_json::from_str(out.trim()).expect("apps JSON");
    let rows = rows.as_array().expect("apps JSON array");
    assert_eq!(rows.len(), 1, "unexpected apps listing: {rows:?}");
    assert_eq!(rows[0]["id"], id);
    assert_eq!(rows[0]["version"], "1.0.0");
    assert_eq!(rows[0]["name"], "Install CI");

    #[cfg(target_os = "linux")]
    {
        let desktop = applications.join(format!("alef-{hash}.desktop"));
        let entry = fs::read_to_string(&desktop).expect("Linux desktop entry");
        assert!(
            entry.contains("Exec=") && entry.contains(" run "),
            "missing run command: {entry}"
        );
        assert!(
            entry.contains(&app_dir.join("1.0.0.alef").display().to_string()),
            "missing precise package path: {entry}"
        );
    }
    #[cfg(target_os = "macos")]
    {
        let bundle = fs::read_dir(&applications)
            .expect("application directory")
            .filter_map(Result::ok)
            .map(|entry| entry.path())
            .find(|path| path.extension().is_some_and(|ext| ext == "app"))
            .expect("installed app bundle");
        let launcher =
            fs::read_to_string(bundle.join("Contents/MacOS/launcher")).expect("app launcher");
        assert!(
            launcher.contains(" run ")
                && launcher.contains(&app_dir.join("1.0.0.alef").display().to_string()),
            "launcher does not reference precise package path: {launcher}"
        );
        assert_eq!(
            fs::read(bundle.join("Contents/Resources/icon.png")).expect("bundle app icon"),
            icon
        );
        let plist =
            fs::read_to_string(bundle.join("Contents/Info.plist")).expect("app bundle plist");
        assert!(
            plist.contains("Install CI"),
            "bundle plist is missing app name: {plist}"
        );
    }
    #[cfg(windows)]
    assert!(
        fs::read_dir(&applications)
            .expect("application directory")
            .filter_map(Result::ok)
            .any(|entry| entry.path().extension().is_some_and(|ext| ext == "lnk")),
        "installed shortcut missing"
    );

    let package_arg = package.display().to_string();
    let (code, out, err) =
        scratch.child(2, &["install", &package_arg, "--trust-key", &fingerprint]);
    assert_eq!(code, 0, "reinstall failed; stdout={out:?}, stderr={err:?}");
    assert!(
        out.contains("already installed"),
        "reinstall was not idempotent: {out:?}"
    );

    let (code, out, err) = scratch.child(3, &["uninstall", &id]);
    assert_eq!(code, 0, "uninstall failed; stdout={out:?}, stderr={err:?}");
    assert!(
        !app_dir.exists(),
        "application store remained after uninstall"
    );
    assert!(package.is_file(), "uninstall removed the source package");
    let trust_pin = home.join("trust").join(format!("{hash}.json"));
    assert!(
        trust_pin.is_file(),
        "normal uninstall removed the trust pin"
    );
    #[cfg(target_os = "linux")]
    assert!(
        !applications.join(format!("alef-{hash}.desktop")).exists(),
        "desktop artifact remained"
    );

    let (code, out, err) = scratch.child(4, &["apps", "--json"]);
    assert_eq!(
        code, 0,
        "apps after uninstall failed; stdout={out:?}, stderr={err:?}"
    );
    assert_eq!(
        serde_json::from_str::<serde_json::Value>(out.trim()).expect("empty apps JSON"),
        serde_json::json!([])
    );

    #[cfg(target_os = "macos")]
    assert!(
        !applications.read_dir().unwrap().any(|entry| entry
            .unwrap()
            .path()
            .extension()
            .is_some_and(|ext| ext == "app")),
        "app bundle remained"
    );
    #[cfg(windows)]
    assert!(
        !applications.read_dir().unwrap().any(|entry| entry
            .unwrap()
            .path()
            .extension()
            .is_some_and(|ext| ext == "lnk")),
        "shortcut remained"
    );

    // Normal uninstall deliberately retains the trust pin; all environment-derived paths were scratch-only.
    assert!(app_home.starts_with(&scratch.0) && applications.starts_with(&scratch.0));
}
