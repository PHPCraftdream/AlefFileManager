// SPDX-License-Identifier: MIT OR Apache-2.0
//! `cli.run` and `cli.start` through the registry: a command the manifest declares, its parameters,
//! the right of its own, and what a substituted or denied command starts (nothing).
use std::time::{Duration, Instant};

use alef_core::{
    ids::StreamId,
    security::consent::{Consent, Decision, Right},
    ErrorCode,
};
use bytes::Bytes;
use serde_json::{json, Value};
use tempfile::tempdir;

use crate::common::Fixture;

use super::{alive, pid_of, pipe::Pipe, until, BASE};

const ARGV: &str = "process.stdout.write(JSON.stringify(process.argv.slice(1)))";

/// One declared command in Ktav; every argument is written as it stands in a list (`:: {name}` for
/// a parameter).
pub fn item(name: &str, program: &str, args: &[&str], description: &str) -> String {
    let args: String = args.iter().map(|a| format!("{:20}{a}\n", "")).collect();
    format!(
        "{:12}{{\n{:16}name: {name}\n{:16}program: {program}\n{:16}args: [\n{args}{:16}]\n{:16}description: {description}\n{:12}}}\n",
        "", "", "", "", "", "", ""
    )
}

/// The commands the tests share.
pub fn items() -> String {
    [
        item(
            "hello",
            "node",
            &[
                "-e",
                "process.stdout.write('hello');process.stderr.write('warn')",
            ],
            "Says hello",
        ),
        item(
            "exit-three",
            "node",
            &["-e", "process.exit(3)"],
            "Exits with 3",
        ),
        item(
            "argv",
            "node",
            &["-e", ARGV, "literal one", ":: {first}", ":: {second}"],
            "Prints its arguments",
        ),
        item(
            "dashed",
            "node",
            &["-e", ARGV, "--", ":: {value}"],
            "Prints a value after the double dash",
        ),
        item(
            "cat",
            "node",
            &["-e", "process.stdin.pipe(process.stdout)"],
            "Copies stdin",
        ),
        item(
            "sleeper",
            "node",
            &["-e", "setInterval(()=>{},1e5)"],
            "Never ends",
        ),
        item(
            "touch",
            "node",
            &[
                "-e",
                "require('fs').writeFileSync(process.argv[1],'x')",
                ":: {file}",
            ],
            "Writes a file",
        ),
        item(
            "env",
            "node",
            &["-e", "process.stdout.write(process.env.ALEF_X||'unset')"],
            "Prints a variable",
        ),
        item(
            "cwd",
            "node",
            &["-e", "process.stdout.write(process.cwd())"],
            "Prints the folder",
        ),
    ]
    .concat()
}

/// An application with the given `cli.exec` list, perhaps one `fs.read` scope, and the shared
/// commands plus `more` of them.
pub fn manifest_with(exec: &[&str], read: Option<&str>, more: &str) -> String {
    let mut text = BASE.replace('\r', "").replace(
        "        exec: []",
        &format!(
            "        exec: [ {} ]\n        commands: [\n{}{more}        ]",
            exec.join(", "),
            items()
        ),
    );
    if let Some(read) = read {
        text = text.replace("        read: []", &format!("        read: [ {read} ]"));
    }
    text
}

async fn app(exec: &[&str]) -> Fixture {
    Fixture::new(Some(&manifest_with(exec, None, "")), &[]).await
}

async fn run(app: &Fixture, args: Value) -> Result<Value, alef_core::AlefError> {
    tokio::time::timeout(Duration::from_secs(30), app.call("cli.run", args))
        .await
        .expect("the run ends in time")
}

async fn start(app: &Fixture, args: Value) -> Result<Value, alef_core::AlefError> {
    tokio::time::timeout(Duration::from_secs(30), app.call("cli.start", args))
        .await
        .expect("the start answers in time")
}

async fn waited(app: &Fixture, process: &Value) -> Result<Value, alef_core::AlefError> {
    tokio::time::timeout(
        Duration::from_secs(20),
        app.call("cli.wait", json!({ "process": process })),
    )
    .await
    .expect("the wait ends in time")
}

fn decided(consent_of: &str, decision: Decision) -> Consent {
    let mut consent = Consent::allow_all();
    consent.set(Right::scoped("cli.command", consent_of), decision);
    consent
}

#[tokio::test]
async fn run_reports_stdout_stderr_and_the_exit_code_like_exec() {
    let fixture = app(&["node"]).await;
    let reply = run(&fixture, json!({ "name": "hello" })).await.unwrap();
    assert_eq!(reply["code"], 0);
    assert_eq!(reply["stdout"], "hello");
    assert_eq!(reply["stderr"], "warn");
    assert!(reply["signal"].is_null());
    let reply = run(&fixture, json!({ "name": "exit-three", "params": {} }))
        .await
        .unwrap();
    assert_eq!(reply["code"], 3);
}

#[tokio::test]
async fn run_passes_a_value_with_spaces_as_one_argument_and_keeps_the_literals() {
    let fixture = app(&["node"]).await;
    let reply = run(
        &fixture,
        json!({ "name": "argv", "params": { "first": "a b  c", "second": "version" } }),
    )
    .await
    .unwrap();
    assert_eq!(reply["code"], 0);
    assert_eq!(
        serde_json::from_str::<Value>(reply["stdout"].as_str().unwrap()).unwrap(),
        json!(["literal one", "a b  c", "version"])
    );
    // Shell syntax and quotes in a value are only text.
    let reply = run(
        &fixture,
        json!({ "name": "argv", "params": { "first": "\"x\" && $(y); {second}", "second": "" } }),
    )
    .await
    .unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(reply["stdout"].as_str().unwrap()).unwrap(),
        json!(["literal one", "\"x\" && $(y); {second}", ""])
    );
}

#[tokio::test]
async fn run_feeds_stdin_from_the_body() {
    let fixture = app(&[]).await;
    let reply = tokio::time::timeout(
        Duration::from_secs(20),
        fixture.call_reply(
            "cli.run",
            json!({ "name": "cat" }),
            Some(Bytes::from_static(b"hello body")),
        ),
    )
    .await
    .expect("the run ends in time")
    .expect("the run works");
    let alef_core::registry::command::Reply::Json(reply) = reply else {
        panic!("expected JSON");
    };
    assert_eq!(reply["stdout"], "hello body");
}

#[tokio::test]
async fn run_times_out_and_the_command_needs_no_cli_exec() {
    let fixture = app(&[]).await;
    let started = Instant::now();
    let error = run(&fixture, json!({ "name": "sleeper", "timeoutMs": 500 }))
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::Timeout);
    assert!(started.elapsed() < Duration::from_secs(20));
    let error = fixture
        .call("cli.exec", json!({ "commandLine": "node -v" }))
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::PermissionDenied);
}

#[tokio::test]
async fn a_missing_unknown_or_nul_parameter_is_invalid_for_run_and_start() {
    let fixture = app(&[]).await;
    let bad = [
        json!({ "name": "argv", "params": { "first": "a" } }),
        json!({ "name": "argv" }),
        json!({ "name": "argv", "params": { "first": "a", "second": "b", "third": "c" } }),
        json!({ "name": "argv", "params": { "first": "a\u{0}b", "second": "b" } }),
        json!({ "name": "hello", "params": { "extra": "x" } }),
        json!({ "name": "argv", "params": { "first": 1, "second": "b" } }),
    ];
    for args in bad {
        for command in ["cli.run", "cli.start"] {
            let error = fixture.call(command, args.clone()).await.unwrap_err();
            assert_eq!(error.code, ErrorCode::InvalidArgument, "{command} {args}");
        }
    }
}

#[tokio::test]
async fn a_page_cannot_choose_the_program_or_add_arguments() {
    let fixture = app(&[]).await;
    for (command, extra) in [
        ("cli.run", json!({ "name": "hello", "args": ["--evil"] })),
        ("cli.run", json!({ "name": "hello", "program": "node" })),
        (
            "cli.run",
            json!({ "name": "hello", "commandLine": "node -v" }),
        ),
        ("cli.start", json!({ "name": "hello", "args": ["--evil"] })),
        ("cli.start", json!({ "name": "hello", "program": "node" })),
        ("cli.start", json!({ "name": "hello", "timeoutMs": 5 })),
    ] {
        let error = fixture.call(command, extra.clone()).await.unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidArgument, "{command} {extra}");
    }
}

#[tokio::test]
async fn a_name_outside_the_list_is_permission_denied() {
    let fixture = app(&["*"]).await;
    for name in ["nope", "node", "", "Hello", "hello ", "../hello"] {
        for command in ["cli.run", "cli.start"] {
            let error = fixture
                .call(command, json!({ "name": name }))
                .await
                .unwrap_err();
            assert_eq!(
                error.code,
                ErrorCode::PermissionDenied,
                "{command} {name:?}"
            );
        }
    }
}

#[tokio::test]
async fn cli_exec_does_not_open_commands_and_a_denied_command_does_not_close_exec() {
    // Rights for programs, no declared commands.
    let plain = Fixture::new(Some(&super::manifest(&["*"])), &[]).await;
    let error = plain
        .call("cli.run", json!({ "name": "hello" }))
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::PermissionDenied);

    let fixture = app(&["node", "*"])
        .await
        .with_consent(decided("hello", Decision::Deny));
    for command in ["cli.run", "cli.start"] {
        let error = fixture
            .call(command, json!({ "name": "hello" }))
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::PermissionDenied, "{command}");
        assert_eq!(error.details.as_ref().unwrap()["permission"], "cli.command");
    }
    let reply = run(&fixture, json!({ "name": "exit-three" }))
        .await
        .unwrap();
    assert_eq!(reply["code"], 3, "another command is not affected");
    let reply = fixture
        .call("cli.exec", json!({ "commandLine": "node -v" }))
        .await
        .unwrap();
    assert_eq!(reply["code"], 0);
}

#[tokio::test]
async fn a_denied_command_starts_nothing() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("made");
    let fixture = app(&["node"])
        .await
        .with_consent(decided("touch", Decision::Deny));
    for command in ["cli.run", "cli.start"] {
        let error = fixture
            .call(
                command,
                json!({ "name": "touch", "params": { "file": file.to_string_lossy() } }),
            )
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::PermissionDenied, "{command}");
    }
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(!file.exists());
}

#[tokio::test]
async fn a_substituted_run_hangs_until_its_timeout_and_starts_nothing() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("made");
    let fixture = app(&["node"])
        .await
        .with_consent(decided("touch", Decision::Substitute));
    let started = Instant::now();
    let error = run(
        &fixture,
        json!({ "name": "touch", "params": { "file": file.to_string_lossy() }, "timeoutMs": 300 }),
    )
    .await
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::Timeout);
    assert!(started.elapsed() >= Duration::from_millis(250));
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the call took {:?}, as if it had started something",
        started.elapsed()
    );
    // A command with bad parameters is no different from a good one while substituted.
    let error = run(&fixture, json!({ "name": "touch", "timeoutMs": 100 }))
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::Timeout);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(!file.exists());
}

#[tokio::test]
async fn a_substituted_start_hangs_for_the_default_time_and_starts_nothing() {
    let dir = tempdir().unwrap();
    let file = dir.path().join("made");
    let fixture = app(&["node"])
        .await
        .with_consent(decided("touch", Decision::Substitute));
    // The 30 s of the default pass on the paused clock of the test.
    tokio::time::pause();
    let before = tokio::time::Instant::now();
    let error = tokio::time::timeout(
        Duration::from_secs(31),
        fixture.call(
            "cli.start",
            json!({ "name": "touch", "params": { "file": file.to_string_lossy() } }),
        ),
    )
    .await
    .expect("the substituted start has a deadline")
    .unwrap_err();
    assert!(before.elapsed() >= Duration::from_secs(30));
    assert!(before.elapsed() < Duration::from_secs(31));
    tokio::time::resume();
    assert_eq!(error.code, ErrorCode::Timeout);
    tokio::time::sleep(Duration::from_millis(500)).await;
    assert!(!file.exists());
}

#[tokio::test]
async fn a_page_adds_nothing_to_the_environment_of_a_command() {
    for exec in [&["node"][..], &["*"][..]] {
        let fixture = app(exec).await;
        for pair in [
            json!(["ALEF_X", "marker"]),
            json!(["GIT_CONFIG_GLOBAL", "x"]),
            json!(["NODE_OPTIONS", "--require x"]),
            json!(["PATH", "x"]),
        ] {
            for command in ["cli.run", "cli.start"] {
                let error = fixture
                    .call(command, json!({ "name": "env", "env": [pair] }))
                    .await
                    .unwrap_err();
                assert_eq!(error.code, ErrorCode::InvalidArgument, "{command} {pair}");
            }
        }
    }
    let fixture = app(&[]).await;
    let reply = run(&fixture, json!({ "name": "env" })).await.unwrap();
    assert_eq!(reply["stdout"], "unset");
}

#[tokio::test]
async fn a_value_starting_with_a_hyphen_is_an_option_unless_the_template_ended_them() {
    let fixture = app(&["node"]).await;
    for params in [
        json!({ "first": "-x", "second": "b" }),
        json!({ "first": "a", "second": "--version" }),
        json!({ "first": "-", "second": "b" }),
        json!({ "first": "--", "second": "b" }),
    ] {
        for command in ["cli.run", "cli.start"] {
            let error = fixture
                .call(command, json!({ "name": "argv", "params": params }))
                .await
                .unwrap_err();
            assert_eq!(error.code, ErrorCode::InvalidArgument, "{command} {params}");
            assert!(error.message.contains("hyphen"), "{}", error.message);
        }
    }
    let reply = run(
        &fixture,
        json!({ "name": "argv", "params": { "first": "a-b", "second": "b-" } }),
    )
    .await
    .unwrap();
    assert_eq!(reply["code"], 0);
    for value in ["--version", "-x", "-"] {
        let reply = run(
            &fixture,
            json!({ "name": "dashed", "params": { "value": value } }),
        )
        .await
        .unwrap();
        assert_eq!(
            serde_json::from_str::<Value>(reply["stdout"].as_str().unwrap()).unwrap(),
            json!([value]),
            "after the double dash a value is an argument"
        );
    }
}

#[tokio::test]
async fn a_cwd_outside_fs_read_is_denied_and_a_missing_one_is_invalid() {
    let allowed = tempdir().unwrap();
    let scope = format!("{}/**", allowed.path().to_string_lossy().replace('\\', "/"));
    let fixture = Fixture::new(Some(&manifest_with(&[], Some(&scope), "")), &[]).await;
    let other = tempdir().unwrap();
    for command in ["cli.run", "cli.start"] {
        let error = fixture
            .call(
                command,
                json!({ "name": "cwd", "cwd": other.path().to_string_lossy() }),
            )
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::PermissionDenied, "{command}");
        let error = fixture
            .call(
                command,
                json!({ "name": "cwd", "cwd": allowed.path().join("missing").to_string_lossy() }),
            )
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidArgument, "{command}");
    }
    let reply = run(
        &fixture,
        json!({ "name": "cwd", "cwd": allowed.path().to_string_lossy() }),
    )
    .await
    .unwrap();
    let seen = std::fs::canonicalize(reply["stdout"].as_str().unwrap()).unwrap();
    assert_eq!(seen, std::fs::canonicalize(allowed.path()).unwrap());
}

#[tokio::test]
async fn a_substituted_cwd_is_denied_for_a_command() {
    let dir = tempdir().unwrap();
    let scope = format!("{}/**", dir.path().to_string_lossy().replace('\\', "/"));
    let mut consent = Consent::allow_all();
    consent.set(Right::scoped("fs.read", &scope), Decision::Substitute);
    let fixture = Fixture::new(Some(&manifest_with(&[], Some(&scope), "")), &[])
        .await
        .with_consent(consent);
    for command in ["cli.run", "cli.start"] {
        let error = fixture
            .call(
                command,
                json!({ "name": "cwd", "cwd": dir.path().to_string_lossy() }),
            )
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::PermissionDenied);
    }
}

#[tokio::test]
async fn start_wires_streams_and_wait_gives_the_exit_once() {
    let fixture = app(&[]).await;
    let reply = start(
        &fixture,
        json!({ "name": "cat", "stdin": "pipe", "stdout": "pipe", "stderr": "ignore" }),
    )
    .await
    .unwrap();
    assert!(reply["process"].as_u64().unwrap() > 0);
    assert!(reply["pid"].as_u64().unwrap() > 0);
    assert!(reply["stdin"].as_u64().is_some());
    assert!(reply["stdout"].as_u64().is_some());
    assert!(reply["stderr"].is_null());
    let writer = fixture
        .session()
        .streams()
        .incoming_writer(StreamId(reply["stdin"].as_u64().unwrap()))
        .expect("an incoming stream");
    tokio::time::timeout(Duration::from_secs(20), async {
        writer.write(Bytes::from_static(b"ping")).await.unwrap();
        writer.end();
    })
    .await
    .expect("the child takes its input");
    let mut pipe = Pipe::open(&fixture, reply["stdout"].as_u64().unwrap());
    assert_eq!(pipe.until_end().await.unwrap(), b"ping");
    let done = waited(&fixture, &reply["process"]).await.unwrap();
    assert_eq!(done["code"], 0);
    assert_eq!(
        waited(&fixture, &reply["process"]).await.unwrap_err().code,
        ErrorCode::NotFound
    );

    let reply = start(
        &fixture,
        json!({ "name": "exit-three", "stdout": "ignore" }),
    )
    .await
    .unwrap();
    assert_eq!(
        waited(&fixture, &reply["process"]).await.unwrap()["code"],
        3
    );
}

#[tokio::test]
async fn start_expands_parameters_without_splitting_values_or_changing_literals() {
    let fixture = app(&[]).await;
    let reply = start(
        &fixture,
        json!({
            "name": "argv", "params": {"first": "a b", "second": ""},
            "stdin": "ignore", "stderr": "ignore"
        }),
    )
    .await
    .unwrap();
    let mut pipe = Pipe::open(&fixture, reply["stdout"].as_u64().unwrap());
    let out = pipe.until_end().await.unwrap();
    assert_eq!(
        serde_json::from_slice::<Value>(&out).unwrap(),
        json!(["literal one", "a b", ""])
    );
    assert_eq!(
        waited(&fixture, &reply["process"]).await.unwrap()["code"],
        0
    );
}

#[tokio::test]
async fn kill_ends_a_started_command_and_wait_reports_it() {
    let fixture = app(&["node"]).await;
    let reply = start(&fixture, json!({ "name": "sleeper" })).await.unwrap();
    let pid = reply["pid"].as_u64().unwrap();
    assert!(alive(&fixture, pid).await);
    fixture
        .call("cli.kill", json!({ "process": reply["process"] }))
        .await
        .expect("the kill works");
    let done = waited(&fixture, &reply["process"]).await.unwrap();
    if cfg!(windows) {
        assert!(!done["code"].is_null(), "{done}");
    } else {
        assert!(done["code"].is_null(), "{done}");
        assert_eq!(done["signal"], "SIGKILL");
    }
    until(
        || async { !alive(&fixture, pid).await },
        15_000,
        "the process is dead",
    )
    .await;
    let error = fixture
        .call("cli.kill", json!({ "process": reply["process"] }))
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::NotFound);
}

#[tokio::test]
async fn closing_the_session_kills_a_started_command() {
    let fixture = app(&["node"]).await;
    let dir = tempdir().unwrap();
    let file = dir.path().join("pid");
    let script =
        "require('fs').writeFileSync(process.argv[1],String(process.pid));setInterval(()=>{},1e5)";
    let more = item(
        "pidfile",
        "node",
        &["-e", script, ":: {file}"],
        "Writes its pid",
    );
    let fixture_with_more = Fixture::new(Some(&manifest_with(&["node"], None, &more)), &[]).await;
    let reply = start(
        &fixture_with_more,
        json!({ "name": "pidfile", "params": { "file": file.to_string_lossy() } }),
    )
    .await
    .unwrap();
    until(|| async { pid_of(&file).is_some() }, 10_000, "a pid file").await;
    let pid = pid_of(&file).unwrap();
    assert_eq!(pid, reply["pid"].as_u64().unwrap());
    assert!(alive(&fixture, pid).await);
    drop(fixture_with_more);
    until(
        || async { !alive(&fixture, pid).await },
        15_000,
        "the process is dead",
    )
    .await;
}
