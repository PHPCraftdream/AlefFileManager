// SPDX-License-Identifier: MIT OR Apache-2.0
//! `sidecar:<name>`: the program `bin/<name>` of the application, as a declared command, in
//! `cli.spawn` and in `cli.exec`, and never a program found on `PATH`. The sidecar is a copy of the
//! `node` of the machine that sits in the folder of an application.
use std::{path::PathBuf, time::Duration};

use alef_core::ErrorCode;
use serde_json::{json, Value};
use tempfile::{tempdir, TempDir};

use crate::common::Fixture;

use super::{
    commands::{item, manifest_with},
    pipe::Pipe,
};

const NAME: &str = "nodecopy";
const SIDE: &str = "sidecar:nodecopy";
const SCRIPT: &str = "process.stdout.write(process.execPath)";

/// The `node` the tests run: the first one on `PATH`.
fn node() -> PathBuf {
    let path = std::env::var_os("PATH").expect("a PATH");
    let names: &[&str] = if cfg!(windows) {
        &["node.exe"]
    } else {
        &["node"]
    };
    std::env::split_paths(&path)
        .flat_map(|dir| names.iter().map(move |name| dir.join(name)))
        .find(|candidate| candidate.is_file())
        .expect("node is on the PATH")
}

/// A fresh application root; hardlinks avoid copying Node for each test.
fn root() -> TempDir {
    let root = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let bin = root.path().join("app").join("bin");
    std::fs::create_dir_all(&bin).unwrap();
    let copy = bin.join(file_name());
    let source = node();
    if std::fs::hard_link(&source, &copy).is_err() {
        std::fs::copy(&source, &copy).unwrap();
    }
    root
}

/// `nodecopy`, with the extension the platform wants.
fn file_name() -> String {
    format!("{NAME}{}", if cfg!(windows) { ".exe" } else { "" })
}

/// Whether `text` (the output of the sidecar) is the path of the copy.
fn is_the_copy(app: &Fixture, text: &str) -> bool {
    let expected = app.context.paths.app.join("bin").join(file_name());
    std::fs::canonicalize(text).expect("the executed file exists")
        == std::fs::canonicalize(expected).unwrap()
}

fn side_item() -> String {
    item(
        "side",
        SIDE,
        &[
            "-e",
            "process.stdout.write(process.execPath+'|'+process.argv[1])",
            ":: {value}",
        ],
        "Runs the sidecar",
    ) + &item(
        "side-sleeper",
        SIDE,
        &["-e", "setInterval(()=>{},1e5)"],
        "Never ends",
    )
}

async fn app(exec: &[&str]) -> (TempDir, Fixture) {
    let root = root();
    let manifest = manifest_with(exec, None, &side_item());
    let fixture = Fixture::new_in(root.path(), Some(&manifest), &[]).await;
    (root, fixture)
}

async fn exec(app: &Fixture, line: &str) -> Result<Value, alef_core::AlefError> {
    tokio::time::timeout(
        Duration::from_secs(30),
        app.call("cli.exec", json!({ "commandLine": line })),
    )
    .await
    .expect("the run ends in time")
}

async fn spawn(app: &Fixture, program: &str) -> Result<Value, alef_core::AlefError> {
    app.call(
        "cli.spawn",
        json!({ "program": program, "args": ["-e", SCRIPT], "stdout": "pipe", "stdin": "ignore", "stderr": "ignore" }),
    )
    .await
}

/// The output of a spawned sidecar and its exit.
async fn finished(app: &Fixture, reply: &Value) -> (String, Value) {
    let mut pipe = Pipe::open(app, reply["stdout"].as_u64().unwrap());
    let out = pipe.until_end().await.expect("the pipe does not break");
    let done = tokio::time::timeout(
        Duration::from_secs(20),
        app.call("cli.wait", json!({ "process": reply["process"] })),
    )
    .await
    .expect("the wait ends in time")
    .expect("the wait works");
    (String::from_utf8(out).unwrap(), done)
}

fn command_line() -> String {
    format!(r#"sidecar:{NAME} -e "{SCRIPT}""#)
}

#[tokio::test]
async fn a_declared_command_runs_a_sidecar_without_cli_exec() {
    let (_dir, fixture) = app(&[]).await;
    let reply = fixture
        .call(
            "cli.run",
            json!({ "name": "side", "params": { "value": "a b" } }),
        )
        .await
        .unwrap();
    assert_eq!(reply["code"], 0, "{reply}");
    let (path, value) = reply["stdout"].as_str().unwrap().split_once('|').unwrap();
    assert!(is_the_copy(&fixture, path), "{path}");
    assert_eq!(value, "a b");
}

#[tokio::test]
async fn a_declared_command_starts_a_sidecar() {
    let (_dir, fixture) = app(&[]).await;
    let reply = fixture
        .call(
            "cli.start",
            json!({ "name": "side", "params": { "value": "v" }, "stdout": "pipe", "stdin": "ignore" }),
        )
        .await
        .unwrap();
    let (out, done) = finished(&fixture, &reply).await;
    assert_eq!(done["code"], 0);
    assert!(
        is_the_copy(&fixture, out.split('|').next().unwrap()),
        "{out}"
    );
}

#[tokio::test]
async fn spawn_and_exec_run_a_sidecar_when_exec_lists_exactly_its_name() {
    let (_dir, fixture) = app(&[SIDE]).await;
    let reply = spawn(&fixture, SIDE).await.unwrap();
    let (out, done) = finished(&fixture, &reply).await;
    assert_eq!(done["code"], 0);
    assert!(is_the_copy(&fixture, &out), "{out}");
    let reply = exec(&fixture, &command_line()).await.unwrap();
    assert_eq!(reply["code"], 0);
    assert!(
        is_the_copy(&fixture, reply["stdout"].as_str().unwrap()),
        "{reply}"
    );
}

#[tokio::test]
async fn the_right_star_allows_a_sidecar_too() {
    let (_dir, fixture) = app(&["*"]).await;
    let reply = spawn(&fixture, SIDE).await.unwrap();
    let (out, _) = finished(&fixture, &reply).await;
    assert!(is_the_copy(&fixture, &out), "{out}");
    let reply = exec(&fixture, &command_line()).await.unwrap();
    assert!(
        is_the_copy(&fixture, reply["stdout"].as_str().unwrap()),
        "{reply}"
    );
}

#[tokio::test]
async fn a_sidecar_is_not_allowed_by_another_name_or_by_the_bare_name() {
    for listed in [
        vec!["nodecopy"],
        vec!["node"],
        vec!["sidecar:other"],
        vec![],
    ] {
        let (_dir, fixture) = app(&listed).await;
        let error = exec(&fixture, &command_line()).await.unwrap_err();
        assert_eq!(error.code, ErrorCode::PermissionDenied, "{listed:?}");
        let error = spawn(&fixture, SIDE).await.unwrap_err();
        assert_eq!(error.code, ErrorCode::PermissionDenied, "{listed:?}");
    }
    // The other way round: the right to a sidecar is not the right to a program of that name.
    let (_dir, fixture) = app(&[SIDE]).await;
    let error = exec(&fixture, &format!("{NAME} -v")).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::PermissionDenied);
    let error = exec(&fixture, "node -v").await.unwrap_err();
    assert_eq!(error.code, ErrorCode::PermissionDenied);
}

#[tokio::test]
async fn a_sidecar_is_never_looked_for_on_the_search_path() {
    // `node` is on the PATH, and `bin/node` is not in the application.
    let (_dir, fixture) = app(&["*"]).await;
    let error = exec(&fixture, "sidecar:node -v").await.unwrap_err();
    assert_eq!(error.code, ErrorCode::NotFound);
    let error = spawn(&fixture, "sidecar:node").await.unwrap_err();
    assert_eq!(error.code, ErrorCode::NotFound);
    // And a bare name that is only a sidecar is not found on the PATH.
    let (_dir, fixture) = app(&[NAME]).await;
    let error = exec(&fixture, &format!("{NAME} -v")).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::NotFound);
    let error = spawn(&fixture, NAME).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::NotFound);
}

#[tokio::test]
async fn a_missing_sidecar_is_not_found_everywhere() {
    let more = item("gone", "sidecar:absent", &[], "Not shipped") + &side_item();
    let manifest = manifest_with(&["*"], None, &more);
    let empty = tempdir().unwrap();
    let fixture = Fixture::new_in(empty.path(), Some(&manifest), &[]).await;
    // A folder is no program.
    std::fs::create_dir_all(empty.path().join("app").join("bin").join("folder")).unwrap();
    for name in ["absent", "folder", NAME] {
        let program = format!("sidecar:{name}");
        let error = exec(&fixture, &program).await.unwrap_err();
        assert_eq!(error.code, ErrorCode::NotFound, "{program}");
        let error = spawn(&fixture, &program).await.unwrap_err();
        assert_eq!(error.code, ErrorCode::NotFound, "{program}");
    }
    for command in ["cli.run", "cli.start"] {
        let error = fixture
            .call(command, json!({ "name": "gone" }))
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::NotFound, "{command}");
    }
    let error = fixture
        .call(
            "cli.run",
            json!({ "name": "side", "params": { "value": "x" } }),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::NotFound);
}

#[tokio::test]
async fn a_bad_sidecar_name_is_invalid_when_allowed_and_denied_when_not_listed() {
    let (_dir, fixture) = app(&["*"]).await;
    for program in [
        "sidecar:",
        "sidecar:..",
        "sidecar:a/b",
        "sidecar:../nodecopy",
        "sidecar:.x",
        "sidecar:a\\b",
    ] {
        let error = exec(&fixture, program).await.unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidArgument, "{program}");
        let error = spawn(&fixture, program).await.unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidArgument, "{program}");
    }
    let error = spawn(&fixture, "sidecar:a b").await.unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidArgument);
    let (_dir, fixture) = app(&[SIDE]).await;
    for program in [
        "sidecar:",
        "sidecar:..",
        "sidecar:a/b",
        "sidecar:../nodecopy",
    ] {
        let error = exec(&fixture, program).await.unwrap_err();
        assert_eq!(error.code, ErrorCode::PermissionDenied, "{program}");
        let error = spawn(&fixture, program).await.unwrap_err();
        assert_eq!(error.code, ErrorCode::PermissionDenied, "{program}");
    }
}

#[tokio::test]
async fn a_shell_cannot_run_a_sidecar() {
    let shell = if cfg!(windows) { "cmd.exe" } else { "sh" };
    for listed in [vec!["*"], vec![SIDE, shell]] {
        let (_dir, fixture) = app(&listed).await;
        let error = fixture
            .call(
                "cli.exec",
                json!({ "commandLine": format!("sidecar:{NAME} -v"), "shell": true }),
            )
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidArgument, "{listed:?}");
        assert!(error.message.contains("shell"), "{}", error.message);
    }
}

#[tokio::test]
async fn a_started_sidecar_is_killed_like_any_process() {
    let (_dir, fixture) = app(&[]).await;
    let reply = fixture
        .call(
            "cli.start",
            json!({ "name": "side-sleeper", "stdout": "ignore", "stdin": "ignore" }),
        )
        .await
        .unwrap();
    fixture
        .call("cli.kill", json!({ "process": reply["process"] }))
        .await
        .expect("the kill works");
    let done = tokio::time::timeout(
        Duration::from_secs(20),
        fixture.call("cli.wait", json!({ "process": reply["process"] })),
    )
    .await
    .expect("the wait ends in time")
    .unwrap();
    assert!(done["code"] != 0, "{done}");
}
