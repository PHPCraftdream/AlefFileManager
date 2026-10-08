// SPDX-License-Identifier: MIT OR Apache-2.0
//! `cli.exec` through the registry: what a run gives back, the rights it needs, and that a tree the
//! runtime kills stays dead.
use std::time::{Duration, Instant};

use alef_core::{
    security::consent::{Consent, Decision, Right},
    ErrorCode,
};
use bytes::Bytes;
use serde_json::json;
use tempfile::tempdir;

use crate::common::Fixture;

use super::{alive, manifest, manifest_with_read, pid_of, until};

async fn app(exec: &[&str]) -> Fixture {
    Fixture::new(Some(&manifest(exec)), &[]).await
}

async fn exec(app: &Fixture, command_line: &str) -> serde_json::Value {
    app.call("cli.exec", json!({ "commandLine": command_line }))
        .await
        .expect("the run itself works")
}

#[tokio::test]
async fn exec_reports_stdout_stderr_and_the_exit_code() {
    let fixture = app(&["node"]).await;
    let reply = exec(
        &fixture,
        r#"node -e "console.log('out'); console.error('err'); process.exit(3)""#,
    )
    .await;
    assert_eq!(reply["code"], 3);
    assert_eq!(reply["stdout"], "out\n");
    assert_eq!(reply["stderr"], "err\n");
    assert!(reply["signal"].is_null());
}

#[tokio::test]
async fn exec_feeds_stdin_from_the_body() {
    let fixture = app(&["node"]).await;
    let reply = fixture
        .call_reply(
            "cli.exec",
            json!({ "commandLine": r#"node -e "let d='';process.stdin.on('data',c=>d+=c);process.stdin.on('end',()=>process.stdout.write('got:'+d))""# }),
            Some(Bytes::from_static(b"hello")),
        )
        .await
        .expect("the run works");
    let alef_core::registry::command::Reply::Json(reply) = reply else {
        panic!("expected JSON");
    };
    assert_eq!(reply["stdout"], "got:hello");
}

#[tokio::test]
async fn exec_times_out_and_kills_the_tree() {
    let fixture = app(&["node"]).await;
    let dir = tempdir().unwrap();
    let parent_file = dir.path().join("parent");
    let grand_file = dir.path().join("grand");
    // Failure cleanup is independent of the implementation's tree guard: even a regression
    // that kills only the direct child must not leave the observed grandchild running.
    struct Cleanup(Vec<std::path::PathBuf>);
    impl Drop for Cleanup {
        fn drop(&mut self) {
            for file in &self.0 {
                if let Some(pid) = pid_of(file) {
                    let script = format!("try{{process.kill({pid},'SIGKILL')}}catch(e){{if(e.code!=='ESRCH')throw e}}");
                    let _ = std::process::Command::new("node")
                        .args(["-e", &script])
                        .status();
                }
            }
        }
    }
    let mut cleanup = Cleanup(vec![grand_file.clone(), parent_file.clone()]);
    let script = "const fs=require('fs');fs.writeFileSync(process.env.PARENT_PID,String(process.pid));require('child_process').spawn(process.execPath,['-e',`require('fs').writeFileSync(process.env.GRAND_PID,String(process.pid));setInterval(()=>{},1e5)`],{stdio:'ignore',detached:process.platform==='win32'});setInterval(()=>{},1e5)";
    // Freeze the command's deadline, not OS scheduling. Keeping yield_now runnable prevents
    // Tokio's paused-clock auto-advance while real Node processes reach the readiness barrier.
    tokio::time::pause();
    let mut running = Box::pin(fixture.call("cli.exec", json!({
        "commandLine": format!("node -e \"{script}\""),
        "env": [["PARENT_PID",parent_file.to_string_lossy()],["GRAND_PID",grand_file.to_string_lossy()]],
        "timeoutMs": 60_000
    })));
    let readiness_deadline = Instant::now() + Duration::from_secs(30);
    loop {
        tokio::select! {
            result = &mut running => panic!("exec finished before tree readiness: {result:?}"),
            _ = tokio::task::yield_now() => {}
        }
        if pid_of(&parent_file).is_some() && pid_of(&grand_file).is_some() {
            break;
        }
        assert!(
            Instant::now() < readiness_deadline,
            "Node tree did not reach readiness within 30 real seconds"
        );
    }
    let parent = pid_of(&parent_file).unwrap();
    let grand = pid_of(&grand_file).unwrap();
    assert_ne!(parent, grand);
    // Both processes have executed their own PID writes and remain in their event loops.
    // Only now expire the configured deadline: startup load cannot prevent the observation.
    tokio::time::advance(Duration::from_secs(61)).await;
    tokio::time::resume();
    let error = tokio::time::timeout(Duration::from_secs(20), running)
        .await
        .expect("timed-out exec reaps and returns")
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::Timeout);
    for pid in [parent, grand] {
        until(
            || async { !alive(&fixture, pid).await },
            15_000,
            "timed-out tree member is dead",
        )
        .await;
    }
    cleanup.0.clear();
}

#[tokio::test]
async fn exec_fails_when_either_pipe_exceeds_the_limit() {
    for pipe in ["stdout", "stderr"] {
        let fixture = app(&["node"]).await;
        let dir = tempdir().unwrap();
        let file = dir.path().join("pid");
        // The child never ends on its own: only the limit ends the run.
        let error = tokio::time::timeout(Duration::from_secs(60), fixture
        .call(
            "cli.exec",
            json!({
                "commandLine": format!(r#"node -e "require('fs').writeFileSync(process.env.ALEF_CLI_TEST_PIDFILE,String(process.pid));process.{pipe}.write('x'.repeat(17*1024*1024));setInterval(()=>{{}},1e5)""#),
                "env": [["ALEF_CLI_TEST_PIDFILE", file.to_string_lossy()]]
            }),
        ))
        .await
        .expect("the limit ends the run")
        .unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidArgument);
        assert!(error.message.contains("16 MiB"), "{}", error.message);
        until(|| async { pid_of(&file).is_some() }, 10_000, "a pid file").await;
        let pid = pid_of(&file).unwrap();
        until(
            || async { !alive(&fixture, pid).await },
            10_000,
            "the tree is dead",
        )
        .await;
    }
}

#[tokio::test]
async fn dropping_pending_exec_kills_the_started_tree() {
    let fixture = app(&["node"]).await;
    let dir = tempdir().unwrap();
    let file = dir.path().join("pid");
    let mut running = Box::pin(fixture.call("cli.exec", json!({
        "commandLine": r#"node -e "require('fs').writeFileSync(process.env.ALEF_CLI_TEST_PIDFILE,String(process.pid));setInterval(()=>{},1e5)""#,
        "env": [["ALEF_CLI_TEST_PIDFILE",file.to_string_lossy()]]
    })));
    tokio::select! {
        result = &mut running => panic!("process unexpectedly finished: {result:?}"),
        _ = until(|| async {pid_of(&file).is_some()}, 15_000, "started child") => {}
    }
    let pid = pid_of(&file).unwrap();
    assert!(alive(&fixture, pid).await);
    drop(running);
    until(
        || async { !alive(&fixture, pid).await },
        15_000,
        "aborted child reaped",
    )
    .await;
}

#[tokio::test]
async fn malformed_environment_is_invalid_even_with_wildcard() {
    let fixture = app(&["*"]).await;
    for pair in [
        ["A=B", "value"],
        ["A\0B", "value"],
        ["A", "value\0"],
        ["", "value"],
    ] {
        let error = fixture
            .call("cli.exec", json!({"commandLine":"node -v","env":[pair]}))
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidArgument);
    }
}

#[tokio::test]
async fn wildcard_shell_owns_unmatched_quote_syntax() {
    let fixture = app(&["*"]).await;
    let reply = fixture
        .call(
            "cli.exec",
            json!({"commandLine":"node -v \"", "shell":true}),
        )
        .await
        .unwrap();
    assert!(reply["code"].as_i64().is_some());
}

#[tokio::test]
async fn a_program_outside_the_scope_is_permission_denied() {
    let fixture = app(&["node"]).await;
    let error = fixture
        .call(
            "cli.exec",
            json!({ "commandLine": "definitely-not-allowed-xyz --version" }),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::PermissionDenied);
}

#[tokio::test]
async fn a_missing_program_is_not_found() {
    let fixture = app(&["definitely-missing-xyz"]).await;
    let error = fixture
        .call(
            "cli.exec",
            json!({ "commandLine": "definitely-missing-xyz" }),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::NotFound);
}

#[tokio::test]
async fn the_right_star_allows_any_program() {
    let fixture = app(&["*"]).await;
    let reply = exec(&fixture, "node -v").await;
    assert_eq!(reply["code"], 0);
    let stdout = reply["stdout"].as_str().unwrap();
    assert!(stdout.starts_with('v'), "{stdout}");
}

#[tokio::test]
async fn a_shell_command_with_operators_is_refused_unless_the_right_is_star() {
    let fixture = app(&["node", "cmd.exe", "sh"]).await;
    for line in ["node -v && node -v", r"node a\b", "node \"a b\""] {
        let error = fixture
            .call("cli.exec", json!({ "commandLine": line, "shell": true }))
            .await
            .unwrap_err();
        assert_eq!(error.code, ErrorCode::InvalidArgument, "{line}");
        assert_eq!(
            error.message, "operators need permissions.cli.exec: [*]",
            "{line}"
        );
    }
    let fixture = app(&["*"]).await;
    let reply = fixture
        .call(
            "cli.exec",
            json!({"commandLine": "echo alef-cli-ok", "shell": true}),
        )
        .await
        .unwrap();
    assert_eq!(reply["code"], 0);
    assert!(
        reply["stdout"].as_str().unwrap().contains("alef-cli-ok"),
        "{}",
        reply["stdout"]
    );
}

#[cfg(not(windows))]
#[tokio::test]
async fn powershell_off_windows_is_invalid() {
    let fixture = app(&["*"]).await;
    let error = fixture
        .call(
            "cli.exec",
            json!({ "commandLine": "echo hi", "shell": "powershell" }),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidArgument);
    assert_eq!(error.message, "powershell is only on Windows");
}

#[tokio::test]
async fn an_env_pair_named_path_is_refused_unless_the_right_is_star() {
    let fixture = app(&["node"]).await;
    let error = fixture
        .call(
            "cli.exec",
            json!({
                "commandLine": "node -v",
                "env": [["PATH", "Z:/nowhere"]]
            }),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidArgument);
    assert!(error.message.contains("PATH"), "{}", error.message);

    // With the right `*` the pair is accepted, and resolution still uses the search path of the
    // runtime, not the one of the page.
    let fixture = app(&["*"]).await;
    let reply = fixture
        .call(
            "cli.exec",
            json!({
                "commandLine": r#"node -e "process.stdout.write(process.env.PATH)""#,
                "env": [["PATH", "Z:/nowhere"]]
            }),
        )
        .await
        .unwrap();
    assert_eq!(reply["code"], 0);
    assert_eq!(
        reply["stdout"].as_str().unwrap(),
        "Z:/nowhere",
        "the page's pair is in the child's environment"
    );
    let reply = exec(&fixture, "node -v").await;
    assert_eq!(reply["code"], 0, "the runtime's PATH found node anyway");
    assert!(reply["stdout"].as_str().unwrap().starts_with('v'));

    let reply = fixture
        .call(
            "cli.exec",
            json!({
                "commandLine": r#"node -e "process.stdout.write(process.env.ALEF_CLI_TEST_ENV)""#,
                "env": [["ALEF_CLI_TEST_ENV", "marker"]]
            }),
        )
        .await
        .expect("the run works");
    assert_eq!(reply["stdout"], "marker");
}

#[tokio::test]
async fn an_unlisted_env_pair_still_reaches_the_child() {
    let fixture = app(&["node"]).await;
    let reply = fixture
        .call(
            "cli.exec",
            json!({
                "commandLine": r#"node -e "process.stdout.write(process.env.ALEF_CLI_TEST_ENV)""#,
                "env": [["ALEF_CLI_TEST_ENV", "marker"]]
            }),
        )
        .await
        .expect("the run works");
    assert_eq!(reply["stdout"], "marker");
}

#[tokio::test]
async fn a_cwd_outside_fs_read_is_denied_and_a_missing_one_is_invalid() {
    let allowed = tempdir().unwrap();
    let scope = format!("{}/**", allowed.path().to_string_lossy().replace('\\', "/"));
    let fixture = Fixture::new(Some(&manifest_with_read(&["node"], &scope)), &[]).await;
    let other = tempdir().unwrap();
    let error = fixture
        .call(
            "cli.exec",
            json!({
                "commandLine": "node -v",
                "cwd": other.path().to_string_lossy()
            }),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::PermissionDenied);
    let error = fixture
        .call(
            "cli.exec",
            json!({
                "commandLine": "node -v",
                "cwd": allowed.path().join("missing").to_string_lossy()
            }),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidArgument);
    let reply = exec_with_cwd(&fixture, allowed.path()).await;
    assert_eq!(reply["code"], 0);
    let seen = std::fs::canonicalize(reply["stdout"].as_str().unwrap()).unwrap();
    assert_eq!(seen, std::fs::canonicalize(allowed.path()).unwrap());
}

async fn exec_with_cwd(fixture: &Fixture, cwd: &std::path::Path) -> serde_json::Value {
    fixture
        .call(
            "cli.exec",
            json!({
                "commandLine": r#"node -e "process.stdout.write(process.cwd())""#,
                "cwd": cwd.to_string_lossy()
            }),
        )
        .await
        .expect("the run works")
}

#[tokio::test]
async fn substituted_cwd_never_runs_in_the_real_folder() {
    let dir = tempdir().unwrap();
    let scope = format!("{}/**", dir.path().to_string_lossy().replace('\\', "/"));
    let run = |read: Decision| {
        let scope = scope.clone();
        let cwd = dir.path().to_string_lossy().into_owned();
        async move {
            let mut consent = Consent::undecided();
            consent.set(Right::scoped("cli.exec", "node"), Decision::Allow);
            consent.set(Right::scoped("fs.read", &scope), read);
            let fixture = Fixture::new(Some(&manifest_with_read(&["node"], &scope)), &[])
                .await
                .with_consent(consent);
            fixture
                .call("cli.exec", json!({ "commandLine": "node -v", "cwd": cwd }))
                .await
        }
    };
    // The same folder allowed runs: the refusal below is the substitution, not another right.
    assert_eq!(
        run(Decision::Allow).await.expect("an allowed cwd")["code"],
        0
    );
    let error = run(Decision::Substitute).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::PermissionDenied);
}

#[tokio::test]
async fn shell_requires_its_own_authorization() {
    let fixture = app(&["node"]).await;
    let error = fixture
        .call("cli.exec", json!({"commandLine":"node -v", "shell":true}))
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::PermissionDenied);
}

/// A right the user substituted starts nothing, and the call hangs only as long as it may.
#[tokio::test]
async fn a_substituted_right_hangs_and_times_out() {
    // `cli.spawn` under a substitution hangs for the default of 30 s and there is no way to bound
    // it: the substituted page in the end-to-end tests covers it instead.
    let mut consent = Consent::undecided();
    consent.set(Right::scoped("cli.exec", "node"), Decision::Substitute);
    let fixture = Fixture::new(Some(&manifest(&["node"])), &[])
        .await
        .with_consent(consent);
    let started = Instant::now();
    let error = fixture
        .call(
            "cli.exec",
            json!({ "commandLine": "node -v", "timeoutMs": 300 }),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::Timeout);
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "the call took {:?}, as if it had started something",
        started.elapsed()
    );
}
