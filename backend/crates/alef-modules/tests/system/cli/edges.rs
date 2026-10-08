// SPDX-License-Identifier: MIT OR Apache-2.0
//! The edges of `cli`: a child that ignores its input, descendants that outlive the child, rights
//! the user substituted, and a program that is nowhere.
use std::{
    path::Path,
    time::{Duration, Instant},
};

use alef_core::{
    registry::command::Reply,
    security::consent::{Consent, Decision, Right},
    ErrorCode,
};
use bytes::Bytes;
use serde_json::json;
use tempfile::tempdir;

use crate::common::Fixture;

use super::{manifest, pid_of, until};

/// The beat of a grandchild: it rewrites this file every 50 ms while it lives.
fn beat_of(file: &Path) -> String {
    std::fs::read_to_string(file.with_extension("beat")).unwrap_or_default()
}

/// Whether the grandchild still lives: its beat changes within half a second. A pid would not do, for
/// Windows gives the pid of a dead process to the next one soon.
async fn beating(file: &Path) -> bool {
    let before = beat_of(file);
    tokio::time::sleep(Duration::from_millis(500)).await;
    beat_of(file) != before
}

/// Kills what a failed test left running, whatever the code under test did (only a grandchild that
/// still beats: its pid is surely its own).
struct Leftovers(Vec<std::path::PathBuf>);

impl Drop for Leftovers {
    fn drop(&mut self) {
        for file in &self.0 {
            let before = beat_of(file);
            std::thread::sleep(Duration::from_millis(500));
            if beat_of(file) == before {
                continue;
            }
            if let Some(pid) = pid_of(file) {
                let script = format!("try{{process.kill({pid},'SIGKILL')}}catch(e){{}}");
                let _ = std::process::Command::new("node")
                    .args(["-e", &script])
                    .status();
            }
        }
    }
}

// Detached on Windows only: there node keeps its children in a job of its own that dies with it, which
// would do the work of the tree killer under test. On Unix a detached child starts a session of its own
// and leaves the process group, which no group kill reaches.
const GRANDCHILD: &str = "require('child_process').spawn(process.execPath,['-e',`require('fs').writeFileSync(process.env.GRAND_PID,String(process.pid));setInterval(()=>require('fs').writeFileSync(process.env.GRAND_PID+'.beat',String(Date.now())),50)`],{stdio:'STDIO',detached:process.platform==='win32'}).unref();const t=setInterval(()=>{if(require('fs').existsSync(process.env.GRAND_PID))process.exit(0)},10)";

#[tokio::test]
async fn a_child_that_does_not_read_its_input_still_reports_how_it_ended() {
    let fixture = Fixture::new(Some(&manifest(&["node"])), &[]).await;
    let reply = tokio::time::timeout(
        Duration::from_secs(60),
        fixture.call_reply(
            "cli.exec",
            json!({ "commandLine": r#"node -e "process.exit(3)""# }),
            Some(Bytes::from(vec![7_u8; 200 * 1024])),
        ),
    )
    .await
    .expect("the run ends")
    .expect("an input nobody read is no failure of the run");
    let Reply::Json(reply) = reply else {
        panic!("expected JSON");
    };
    assert_eq!(reply["code"], 3);
}

#[tokio::test]
async fn exec_ends_with_its_child_and_takes_the_descendants_along() {
    let fixture = Fixture::new(Some(&manifest(&["node"])), &[]).await;
    let dir = tempdir().unwrap();
    let grand = dir.path().join("grand");
    let _leftovers = Leftovers(vec![grand.clone()]);
    // The grandchild holds the pipes of the run: only its death ends the reading.
    let script = GRANDCHILD.replace("STDIO", "inherit");
    let reply = tokio::time::timeout(
        Duration::from_secs(60),
        fixture.call(
            "cli.exec",
            json!({
                "commandLine": format!("node -e \"{script}\""),
                "env": [["GRAND_PID", grand.to_string_lossy()]]
            }),
        ),
    )
    .await
    .expect("the run ends with its child")
    .expect("the run works");
    assert_eq!(reply["code"], 0);
    assert!(pid_of(&grand).is_some(), "the grandchild started");
    until(
        || async { !beating(&grand).await },
        60_000,
        "the grandchild died",
    )
    .await;
}

#[tokio::test]
async fn the_descendants_of_a_spawned_child_die_with_it_even_unwaited() {
    let fixture = Fixture::new(Some(&manifest(&["node"])), &[]).await;
    let dir = tempdir().unwrap();
    let grand = dir.path().join("grand");
    let _leftovers = Leftovers(vec![grand.clone()]);
    let script = GRANDCHILD.replace("STDIO", "ignore");
    fixture
        .call(
            "cli.spawn",
            json!({
                "program": "node", "args": ["-e", script],
                "stdin": "ignore", "stdout": "ignore", "stderr": "ignore",
                "env": [["GRAND_PID", grand.to_string_lossy()]]
            }),
        )
        .await
        .expect("the spawn works");
    until(
        || async { pid_of(&grand).is_some() },
        60_000,
        "the grandchild started",
    )
    .await;
    until(
        || async { !beating(&grand).await },
        60_000,
        "the grandchild died",
    )
    .await;
}

fn consent(rights: &[(&str, Decision)]) -> Consent {
    let mut consent = Consent::undecided();
    for (target, decision) in rights {
        consent.set(Right::scoped("cli.exec", target), *decision);
    }
    consent
}

#[tokio::test]
async fn a_substituted_shell_starts_nothing_and_times_out() {
    let fixture = Fixture::new(Some(&manifest(&["node", "cmd.exe", "sh"])), &[])
        .await
        .with_consent(consent(&[
            ("node", Decision::Allow),
            ("cmd.exe", Decision::Substitute),
            ("sh", Decision::Substitute),
        ]));
    // A script that leaves a mark when it runs, in a folder whose path has no character the shell acts
    // on (the temp folder of a runner can be a short name with a tilde).
    let dir = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let script = dir.path().join("run.js");
    std::fs::write(
        &script,
        "require('fs').writeFileSync(__dirname + '/started', 'x')",
    )
    .unwrap();
    let line = format!("node {}", script.to_string_lossy().replace('\\', "/"));
    let started = Instant::now();
    let error = fixture
        .call(
            "cli.exec",
            json!({ "commandLine": line, "shell": true, "timeoutMs": 3000 }),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::Timeout);
    assert!(started.elapsed() < Duration::from_secs(10));
    assert!(
        !dir.path().join("started").exists(),
        "the shell ran the line"
    );
}

#[tokio::test]
async fn a_substituted_spawn_starts_nothing() {
    let fixture = Fixture::new(Some(&manifest(&["node"])), &[])
        .await
        .with_consent(consent(&[("node", Decision::Substitute)]));
    let attempt = tokio::time::timeout(
        Duration::from_secs(1),
        fixture.call("cli.spawn", json!({ "program": "node", "args": ["-v"] })),
    )
    .await;
    assert!(
        attempt.is_err(),
        "a substituted spawn answered: {attempt:?}"
    );
}

#[tokio::test]
async fn a_program_spawn_does_not_find_is_not_found() {
    let fixture = Fixture::new(Some(&manifest(&["*"])), &[]).await;
    let error = fixture
        .call("cli.spawn", json!({ "program": "alef-no-such-program" }))
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::NotFound);
}
