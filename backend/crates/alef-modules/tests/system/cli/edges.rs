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

use super::{manifest, pid_of, pty::plain, until};

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
    let mut running = Box::pin(fixture.call(
        "cli.exec",
        json!({
            "commandLine": format!("node -e \"{script}\""),
            "env": [["GRAND_PID", grand.to_string_lossy()]]
        }),
    ));
    let reply = tokio::time::timeout(Duration::from_secs(60), &mut running)
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

/// A terminal nobody reads is killed: the output kept so far is handed over, the loss is reported
/// after it, and the end comes last.
#[tokio::test]
async fn a_terminal_killed_with_stalled_credit_keeps_its_prefix_and_reports_the_loss() {
    use alef_core::{
        ids::StreamId,
        protocol::{call::Limits, frame::Frame},
    };
    let limits = Limits {
        stream_window: 8192,
        chunk_size: 4096,
        ..Limits::default()
    };
    let app = Fixture::new_limited(Some(&manifest(&["node"])), limits).await;
    // Count completed writes, not writes merely started. A scheduling pause at 250 started rows
    // does not prove the 256 KiB capture buffer is full. 305 completed rows exceed that buffer,
    // the 8 KiB stream window and a 32 KiB chunk held by replay; stalled credit leaves a tail.
    // The worker keeps beating even while the main thread is blocked in a terminal write.
    let script = r"const {Worker}=require('worker_threads');const shared=new Int32Array(new SharedArrayBuffer(4));new Worker(`const {workerData:{shared,file}}=require('worker_threads');let b=0;setInterval(()=>require('fs').writeFileSync(file,Atomics.load(shared,0)+' '+b++),50)`,{eval:true,workerData:{shared,file:process.argv[1]}});const row=Array.from({length:990},(_,c)=>String.fromCharCode(97+(c*13+(c>>3))%26)).join('');let n=0;function pump(){process.stdout.write('ROW'+String(n++).padStart(4,'0')+':'+row+'\r\n',()=>{Atomics.add(shared,0,1);setImmediate(pump)})}process.stdout.write('BEGIN:',pump)";
    let dir = tempdir().unwrap();
    let state = dir.path().join("state");
    let args = json!({"program":"node", "cols":1000, "rows":1000, "args":["-e", script, state.to_str().unwrap()]});
    let opened = app.call("cli.pty", args).await.unwrap();
    let id = StreamId(opened["output"].as_u64().unwrap());
    let session = app.session();
    let mut reader = session.streams().reader(id).unwrap();
    // Completed rows before a stall counts as one that fills the retained slots. A Unix terminal
    // holds the child back by itself after about 270-290 rows. ConPTY absorbs more than the 256 KiB
    // capture buffer, the 8 KiB window and a 32 KiB chunk, so a pause below that is a scheduling one.
    let full = if cfg!(windows) { 305 } else { 250 };
    let held = tokio::time::timeout(Duration::from_secs(30), async {
        let (mut rows, mut since) = (0, 0);
        loop {
            let text = std::fs::read_to_string(&state).unwrap_or_default();
            let mut numbers = text
                .split_whitespace()
                .filter_map(|n| n.parse::<usize>().ok());
            if let (Some(now), Some(beat)) = (numbers.next(), numbers.next()) {
                if now != rows {
                    (rows, since) = (now, beat);
                } else if rows >= full && beat >= since + 6 {
                    break;
                }
            }
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    })
    .await;
    let process = json!({"process": opened["process"]});
    let killed = tokio::time::timeout(
        Duration::from_secs(20),
        app.call("cli.kill", process.clone()),
    )
    .await;
    let waited = tokio::time::timeout(Duration::from_secs(20), app.call("cli.wait", process)).await;
    tokio::time::sleep(Duration::from_secs(1)).await;
    let retained = !session.resources().is_empty();
    let output = tokio::time::timeout(Duration::from_secs(20), async {
        let (mut bytes, mut gaps) = (Vec::new(), Vec::new());
        while let Some(frame) = reader.next_frame().await {
            match frame {
                Frame::Binary(chunk) => {
                    session.streams().ack(id, chunk.len()).unwrap();
                    bytes.extend_from_slice(&chunk);
                }
                Frame::Json(value) => {
                    session.streams().ack(id, value.to_string().len()).unwrap();
                    gaps.push((bytes.len(), value));
                }
                Frame::End => return (bytes, gaps, true),
                Frame::Error(error) => panic!("output failed: {error}"),
            }
        }
        (bytes, gaps, false)
    })
    .await;
    let gone = tokio::time::timeout(Duration::from_secs(20), async {
        while !session.resources().is_empty() {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await;
    // A terminal left open would keep the pseudo console, and with it the test binary, alive.
    tokio::time::timeout(Duration::from_secs(20), session.close())
        .await
        .expect("cleanup completes");
    assert!(
        held.is_ok(),
        "the terminal does not hold the child back; its state: {:?}",
        std::fs::read_to_string(&state)
    );
    killed.expect("kill completes").unwrap();
    waited.expect("wait completes").unwrap();
    assert!(retained, "wait retains stalled output");
    let (bytes, gaps, ended) = output.expect("output completes");
    assert!(plain(&bytes).contains("BEGIN:"), "prefix retained");
    // XNU flushes what the kernel holds when a killed child's slave closes: nothing is left to count.
    if !cfg!(target_os = "macos") {
        assert!(!gaps.is_empty(), "the final drain reports the overflow");
    }
    for (at, gap) in gaps {
        assert!(at > 0, "retained bytes precede gap");
        assert_eq!(gap["kind"], "overflow");
        assert!(gap["dropped"].as_u64().unwrap() > 0);
    }
    assert!(ended, "end follows retained bytes and overflow");
    gone.expect("resource removed after replay");
}
