// SPDX-License-Identifier: MIT OR Apache-2.0
//! The commands of `app`, `path` and `os` through the registry, as the transport calls them.
mod common;

use alef_core::{registry::host::Theme, ErrorCode};
use common::{path_vars, Fixture};
use serde_json::{json, Value};

const BASE_MANIFEST: &str = include_str!("fixtures/app.ktav");

fn without_env(manifest: &str) -> String {
    manifest.replace("env: [ PATH, ALEF_MODULES_TEST_UNSET ]", "env: []")
}

#[tokio::test]
async fn the_command_surface_is_exactly_the_documented_one() {
    let fixture = Fixture::new(None, &[]).await;
    assert_eq!(
        fixture.registry.command_names(),
        [
            "app.args",
            "app.cwd",
            "app.env",
            "app.envAll",
            "app.info",
            "app.quit",
            "app.quitAnswer",
            "app.quitIntercept",
            "app.relaunch",
            "app.requestSingleInstance",
            "clipboard.readHtml",
            "clipboard.readImage",
            "clipboard.readText",
            "clipboard.writeHtml",
            "clipboard.writeImage",
            "clipboard.writeText",
            "dialog.confirm",
            "dialog.message",
            "dialog.open",
            "dialog.save",
            "fs.close",
            "fs.copy",
            "fs.exists",
            "fs.fstat",
            "fs.lstat",
            "fs.mkdir",
            "fs.open",
            "fs.read",
            "fs.readDir",
            "fs.readDirStream",
            "fs.readFile",
            "fs.readStream",
            "fs.remove",
            "fs.rename",
            "fs.settle",
            "fs.stat",
            "fs.sync",
            "fs.tempDir",
            "fs.tempFile",
            "fs.truncate",
            "fs.watch",
            "fs.write",
            "fs.writeFile",
            "fs.writeStream",
            "notification.show",
            "os.info",
            "os.theme",
            "path.appCache",
            "path.appConfig",
            "path.appData",
            "path.basename",
            "path.desktop",
            "path.dirname",
            "path.documents",
            "path.downloads",
            "path.executable",
            "path.home",
            "path.join",
            "path.normalize",
            "path.temp",
            "screen.cursorPosition",
            "screen.monitors",
            "shell.openExternal",
            "shell.openPath",
            "shell.showInFolder",
            "shell.trash",
            "sqlite.close",
            "sqlite.exec",
            "sqlite.finalize",
            "sqlite.iterate",
            "sqlite.open",
            "sqlite.prepare",
            "sqlite.query",
            "store.delete",
            "store.flush",
            "store.get",
            "store.keys",
            "store.open",
            "store.set",
            "window.all",
            "window.center",
            "window.close",
            "window.closeAnswer",
            "window.closeIntercept",
            "window.create",
            "window.destroy",
            "window.focus",
            "window.hide",
            "window.maximize",
            "window.minimize",
            "window.restore",
            "window.setAlwaysOnTop",
            "window.setDecorations",
            "window.setFullscreen",
            "window.setMaxSize",
            "window.setMinSize",
            "window.setPosition",
            "window.setResizable",
            "window.setSize",
            "window.setTitle",
            "window.setZoom",
            "window.show",
            "window.startDrag",
            "window.startResize",
            "window.state",
            "window.toggleMaximize",
        ]
    );
    let mut again = fixture.registry.clone();
    let error = alef_modules::register_all(&mut again, fixture.host.clone(), &fixture.context)
        .expect_err("registering twice");
    assert_eq!(error.code, ErrorCode::AlreadyExists);
}

#[tokio::test]
async fn app_info_args_and_cwd_report_the_process() {
    let fixture = Fixture::new(None, &["--port", "8080", "a.txt", "--", "-b"]).await;
    assert_eq!(
        fixture.call("app.info", Value::Null).await.unwrap(),
        json!({"id": "org.example.modules", "name": "Modules", "version": "2.3.4", "runtimeVersion": "9.9.9"})
    );
    assert_eq!(
        fixture.call("app.args", Value::Null).await.unwrap(),
        json!({
            "raw": ["--port", "8080", "a.txt", "--", "-b"],
            "parsed": {"port": 8080.0},
            "positional": ["a.txt", "-b"],
        })
    );
    assert_eq!(
        fixture.call("app.cwd", Value::Null).await.unwrap(),
        json!(std::env::current_dir().unwrap().to_string_lossy())
    );
}

#[tokio::test]
async fn env_is_limited_to_the_names_the_manifest_lists() {
    let fixture = Fixture::new(None, &[]).await;
    let path = std::env::var("PATH").expect("PATH is set for the test run");
    assert_eq!(
        fixture
            .call("app.env", json!({"name": "PATH"}))
            .await
            .unwrap(),
        json!(path)
    );
    assert_eq!(
        fixture
            .call("app.env", json!({"name": "ALEF_MODULES_TEST_UNSET"}))
            .await
            .unwrap(),
        Value::Null,
        "a listed variable that is not set is null, not an error"
    );
    let all = fixture.call("app.envAll", Value::Null).await.unwrap();
    assert_eq!(
        all,
        json!({"PATH": path}),
        "only listed variables that are set"
    );
    for unlisted in ["HOME", "USERPROFILE", "path", ""] {
        let error = fixture
            .call("app.env", json!({"name": unlisted}))
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::PermissionDenied, "{unlisted:?}");
        assert_eq!(error.details, Some(json!({"permission": "app.env"})));
    }
    let error = fixture.call("app.env", json!({})).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidArgument);
}

#[tokio::test]
async fn without_an_env_list_every_read_is_refused() {
    let fixture = Fixture::new(Some(&without_env(BASE_MANIFEST)), &[]).await;
    for (command, args) in [
        ("app.env", json!({"name": "PATH"})),
        ("app.envAll", Value::Null),
    ] {
        let error = fixture.call(command, args).await.unwrap_err();
        assert_eq!(error.code, ErrorCode::PermissionDenied, "{command}");
        assert_eq!(
            error.details,
            Some(json!({"permission": "app.env"})),
            "{command}"
        );
    }
}

#[tokio::test]
async fn quit_passes_the_exit_code_to_the_host_and_rejects_nonsense() {
    let fixture = Fixture::new(None, &[]).await;
    fixture.call("app.quit", json!({})).await.unwrap();
    fixture.call("app.quit", json!({"code": 7})).await.unwrap();
    assert_eq!(*fixture.host.quits.lock().unwrap(), [0, 7]);
    for bad in [
        json!({"code": 256}),
        json!({"code": -1}),
        json!({"code": 1.5}),
        json!({"code": "1"}),
        json!({"extra": 1}),
    ] {
        let error = fixture.call("app.quit", bad.clone()).await.unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidArgument, "{bad}");
    }
    assert_eq!(
        *fixture.host.quits.lock().unwrap(),
        [0, 7],
        "a refused quit does not quit"
    );
}

#[tokio::test]
async fn path_commands_report_the_directories_of_the_context() {
    let fixture = Fixture::new(None, &[]).await;
    let vars = path_vars(
        &std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .join("alef-modules-paths"),
    );
    for (command, expected) in [
        ("path.appData", &vars.app_data),
        ("path.appConfig", &vars.app_config),
        ("path.appCache", &vars.app_cache),
        ("path.temp", &vars.temp),
        ("path.home", &vars.home),
        ("path.documents", &vars.documents),
        ("path.downloads", &vars.downloads),
        ("path.desktop", &vars.desktop),
    ] {
        assert_eq!(
            fixture.call(command, Value::Null).await.unwrap(),
            json!(expected.to_string_lossy()),
            "{command}"
        );
    }
    assert_eq!(
        fixture.call("path.executable", Value::Null).await.unwrap(),
        json!(std::env::current_exe().unwrap().to_string_lossy())
    );
}

#[tokio::test]
async fn path_arithmetic_is_lexical_and_validates_its_arguments() {
    let fixture = Fixture::new(None, &[]).await;
    let sep = std::path::MAIN_SEPARATOR;
    let joined = fixture
        .call("path.join", json!({"parts": ["a", "b", "..", "c"]}))
        .await
        .unwrap();
    assert_eq!(joined, json!(format!("a{sep}c")));
    assert_eq!(
        fixture
            .call("path.normalize", json!({"path": "x/./y/../z"}))
            .await
            .unwrap(),
        json!(format!("x{sep}z"))
    );
    assert_eq!(
        fixture
            .call("path.dirname", json!({"path": "x/y/z.txt"}))
            .await
            .unwrap(),
        json!(format!("x{sep}y"))
    );
    assert_eq!(
        fixture
            .call("path.basename", json!({"path": "x/y/z.txt"}))
            .await
            .unwrap(),
        json!("z.txt")
    );
    for (command, bad) in [
        ("path.join", json!({"parts": "a"})),
        ("path.join", json!({"parts": [1]})),
        ("path.normalize", json!({})),
        ("path.dirname", json!({"path": 3})),
        ("path.basename", Value::Null),
    ] {
        let error = fixture.call(command, bad.clone()).await.unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidArgument, "{command} {bad}");
    }
}

#[tokio::test]
async fn os_reports_facts_and_the_theme_of_the_host() {
    let fixture = Fixture::new(None, &[]).await;
    let info = fixture.call("os.info", Value::Null).await.unwrap();
    assert_eq!(info["platform"], std::env::consts::OS);
    assert_eq!(info["arch"], std::env::consts::ARCH);
    for field in ["version", "locale", "hostname"] {
        assert!(
            info[field].as_str().is_some_and(|text| !text.is_empty()),
            "{field}: {info}"
        );
    }
    assert_eq!(
        fixture.call("os.theme", Value::Null).await.unwrap(),
        json!("light")
    );
    *fixture.host.theme.lock().unwrap() = Theme::Dark;
    assert_eq!(
        fixture.call("os.theme", Value::Null).await.unwrap(),
        json!("dark")
    );
}
