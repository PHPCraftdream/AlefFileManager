// SPDX-License-Identifier: MIT OR Apache-2.0
use super::{manifest, pipe::Pipe};
use crate::common::Fixture;
use alef_core::{ids::StreamId, ErrorCode};
use bytes::Bytes;
use serde_json::{json, Value};
use std::time::Duration;

async fn call(app: &Fixture, name: &str, args: Value) -> Result<Value, alef_core::AlefError> {
    tokio::time::timeout(Duration::from_secs(20), app.call(name, args))
        .await
        .expect("call completes")
}
async fn close(app: &Fixture) {
    tokio::time::timeout(Duration::from_secs(20), app.session().close())
        .await
        .expect("cleanup completes");
}
/// The code a call is refused with. A terminal that starts when it should not is closed before the test fails: it would
/// otherwise outlive the failure, and the pseudo console blocks the exit of the test binary until its child ends.
async fn refused(app: &Fixture, name: &str, args: Value) -> ErrorCode {
    match call(app, name, args.clone()).await {
        Err(error) => error.code,
        Ok(_) => {
            close(app).await;
            panic!("{name} accepted {args}");
        }
    }
}
#[tokio::test]
async fn terminal_output_respects_credit_and_shutdown_does_not_wait_for_the_page() {
    use alef_core::protocol::credit::DEFAULT_STREAM_WINDOW;
    let app = Fixture::new(Some(&manifest(&["node"])), &[]).await;
    let opened = call(&app,"cli.pty",json!({"program":"node","cols":80,"rows":24,"args":["-e","setInterval(()=>process.stdout.write('x'.repeat(32768)),1)"]})).await.unwrap();
    let id = StreamId(opened["output"].as_u64().unwrap());
    let session = app.session();
    let _reader = session.streams().reader(id).unwrap();
    tokio::time::timeout(Duration::from_secs(20), async {
        while session.streams().outstanding(id).unwrap_or(0) < DEFAULT_STREAM_WINDOW - 32768 {
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("output fills credit window");
    assert!(session.streams().outstanding(id).unwrap() <= DEFAULT_STREAM_WINDOW);
    close(&app).await;
}

#[tokio::test]
async fn a_terminal_reports_its_size_echoes_input_and_resizes() {
    let app = Fixture::new(Some(&manifest(&["node"])), &[]).await;
    let opened = call(&app, "cli.pty", json!({"program":"node","cols":83,"rows":27,"args":["-e", "process.stdin.setRawMode(true);function size(){process.stdout.write('SIZE:'+process.stdout.columns+':'+process.stdout.rows+'\\n')}size();process.stdout.on('resize',size);process.stdin.on('data',b=>{process.stdout.write('ECHO:'+b);if(b.includes(113))process.exit(0)})"]})).await.unwrap();
    let mut pipe = Pipe::open(&app, opened["output"].as_u64().unwrap());
    let writer = app
        .session()
        .streams()
        .incoming_writer(StreamId(opened["input"].as_u64().unwrap()))
        .unwrap();
    let mut seen = Vec::new();
    tokio::time::timeout(Duration::from_secs(20), async {
        while !String::from_utf8_lossy(&seen).contains("SIZE:83:27") {
            seen.extend(pipe.exactly(1).await);
        }
    })
    .await
    .expect("initial size arrives");
    call(
        &app,
        "cli.resize",
        json!({"process":opened["process"],"cols":97,"rows":31}),
    )
    .await
    .unwrap();
    tokio::time::timeout(Duration::from_secs(20), async {
        while !String::from_utf8_lossy(&seen).contains("SIZE:97:31") {
            seen.extend(pipe.exactly(1).await);
        }
    })
    .await
    .expect("resized size arrives");
    tokio::time::timeout(
        Duration::from_secs(20),
        writer.write(Bytes::from_static(b"q")),
    )
    .await
    .unwrap()
    .unwrap();
    seen.extend(
        tokio::time::timeout(Duration::from_secs(20), pipe.until_end())
            .await
            .unwrap()
            .unwrap(),
    );
    assert!(String::from_utf8_lossy(&seen).contains("ECHO:q"));
    assert_eq!(
        call(&app, "cli.wait", json!({"process":opened["process"]}))
            .await
            .unwrap()["code"],
        0
    );
    assert_eq!(
        call(&app, "cli.wait", json!({"process":opened["process"]}))
            .await
            .unwrap_err()
            .code,
        ErrorCode::NotFound
    );
    close(&app).await;
}
#[tokio::test]
async fn terminal_wait_keeps_output_for_a_reader_delayed_more_than_two_seconds() {
    let app = Fixture::new(Some(&manifest(&["node"])), &[]).await;
    let opened = call(&app, "cli.pty", json!({"program":"node","cols":1000,"rows":1000,"args":["-e","process.stdout.write('BEGIN:'+Array.from({length:1200},(_,i)=>'ROW'+String(i).padStart(4,'0')+':'+ 'x'.repeat(80)+'\\n').join('')+':END',()=>process.exit(0))"]})).await.unwrap();
    let mut pipe = Pipe::open(&app, opened["output"].as_u64().unwrap());
    assert_eq!(
        call(&app, "cli.wait", json!({"process":opened["process"]}))
            .await
            .unwrap()["code"],
        0
    );
    tokio::time::sleep(Duration::from_secs(3)).await;
    let bytes = tokio::time::timeout(Duration::from_secs(20), pipe.until_end())
        .await
        .unwrap()
        .unwrap();
    let text = String::from_utf8_lossy(&bytes);
    assert!(text.contains("BEGIN:"), "missing beginning");
    assert!(text.contains(":END"), "missing ending");
    for i in 0..1200 {
        assert!(
            text.contains(&format!("ROW{i:04}:{}", "x".repeat(80))),
            "missing row {i}"
        );
    }
    close(&app).await;
}

#[tokio::test]
async fn terminal_dimensions_and_environment_are_validated() {
    let app = Fixture::new(Some(&manifest(&["node"])), &[]).await;
    for dim in [
        json!(0),
        json!(1001),
        json!(-1),
        json!(1.5),
        json!("80"),
        Value::Null,
    ] {
        for axis in ["cols", "rows"] {
            let mut args = json!({"program":"node","cols":80,"rows":24});
            args[axis] = dim.clone();
            assert_eq!(
                refused(&app, "cli.pty", args).await,
                ErrorCode::InvalidArgument
            );
            let mut args = json!({"process":999,"cols":80,"rows":24});
            args[axis] = dim.clone();
            assert_eq!(
                call(&app, "cli.resize", args).await.unwrap_err().code,
                ErrorCode::InvalidArgument
            );
        }
    }
    for pair in [
        ["PATH", "x"],
        ["LD_PRELOAD", "x"],
        ["A=B", "x"],
        ["A", "x\0"],
    ] {
        assert_eq!(
            refused(
                &app,
                "cli.pty",
                json!({"program":"node","cols":80,"rows":24,"env":[pair]})
            )
            .await,
            ErrorCode::InvalidArgument
        );
    }
    assert_eq!(
        refused(
            &app,
            "cli.pty",
            json!({"program":"not-allowed","cols":80,"rows":24})
        )
        .await,
        ErrorCode::PermissionDenied
    );
    close(&app).await;
}

#[tokio::test]
async fn terminal_trees_stop_on_kill_session_close_and_parent_exit() {
    for ending in ["kill", "session", "exit"] {
        let app = Fixture::new(Some(&manifest(&["node"])), &[]).await;
        let dir = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
        let beat = dir.path().join("beat");
        let done = dir.path().join("done");
        let stop = dir.path().join("stop");
        struct Stop(std::path::PathBuf);
        impl Drop for Stop {
            fn drop(&mut self) {
                let _ = std::fs::write(&self.0, b"stop");
            }
        }
        let _stop = Stop(stop.clone());
        let grand = "const fs=require('fs');let n=0;const deadline=Date.now()+45000;setInterval(()=>{if(Date.now()>=deadline||fs.existsSync(process.env.STOP))process.exit(0);fs.appendFileSync(process.env.BEAT,String(++n)+'\\n')},25)";
        let script = format!("const fs=require('fs');require('child_process').spawn(process.execPath,['-e',{}],{{detached:process.platform==='win32',stdio:'ignore',env:process.env}}).unref();setInterval(()=>{{if(fs.existsSync(process.env.DONE))process.exit(0)}},25)", serde_json::to_string(grand).unwrap());
        let opened = call(&app,"cli.pty",json!({"program":"node","cols":80,"rows":24,"args":["-e",script],"env":[["BEAT",beat.to_string_lossy()],["DONE",done.to_string_lossy()],["STOP",stop.to_string_lossy()]]})).await.unwrap();
        let read = || {
            std::fs::read_to_string(&beat).and_then(|s| {
                s.lines()
                    .rev()
                    .find_map(|line| line.parse::<u64>().ok())
                    .ok_or_else(|| {
                        std::io::Error::new(std::io::ErrorKind::InvalidData, "no complete beat")
                    })
            })
        };
        tokio::time::timeout(Duration::from_secs(20), async {
            while !matches!(read(), Ok(n) if n >= 3) {
                tokio::time::sleep(Duration::from_millis(25)).await;
            }
        })
        .await
        .expect("grandchild is beating");
        if ending == "session" {
            close(&app).await;
        } else {
            if ending == "kill" {
                call(&app, "cli.kill", json!({"process":opened["process"]}))
                    .await
                    .unwrap();
            } else {
                std::fs::write(&done, b"go").unwrap();
            }
            call(&app, "cli.wait", json!({"process":opened["process"]}))
                .await
                .unwrap();
        }
        tokio::time::sleep(Duration::from_millis(300)).await;
        let stopped = read().expect("beat remains readable after termination");
        tokio::time::sleep(Duration::from_millis(300)).await;
        assert_eq!(
            read().expect("beat remains readable"),
            stopped,
            "grandchild keeps beating after {ending}"
        );
        close(&app).await;
    }
}

#[tokio::test]
async fn terminal_cwd_env_and_runtime_resolution_reach_the_child() {
    let dir = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let scope = format!("{}/**", dir.path().to_string_lossy().replace('\\', "/"));
    let app = Fixture::new(Some(&super::manifest_with_read(&["*"], &scope)), &[]).await;
    let opened = call(&app,"cli.pty",json!({"program":"node","cols":1000,"rows":1,"cwd":dir.path().to_string_lossy(),"env":[["PATH","missing"],["ALEF_PTY","marker"]],"args":["-e","process.stdout.write('RESULT:'+JSON.stringify([process.cwd(),process.env.ALEF_PTY,process.env.PATH])+'\\n')"]})).await.unwrap();
    let mut pipe = Pipe::open(&app, opened["output"].as_u64().unwrap());
    let bytes = tokio::time::timeout(Duration::from_secs(20), pipe.until_end())
        .await
        .unwrap()
        .unwrap();
    let text = String::from_utf8_lossy(&bytes);
    assert!(text.contains("marker"), "{text}");
    assert!(text.contains("missing"), "{text}");
    assert!(
        text.contains(
            &dir.path()
                .file_name()
                .unwrap()
                .to_string_lossy()
                .to_string()
        ),
        "{text}"
    );
    assert_eq!(
        call(&app, "cli.wait", json!({"process":opened["process"]}))
            .await
            .unwrap()["code"],
        0
    );
    close(&app).await;
}

#[tokio::test]
async fn a_substituted_terminal_starts_nothing_and_times_out() {
    use alef_core::security::consent::{Consent, Decision, Right};
    let dir = tempfile::tempdir().unwrap();
    let marker = dir.path().join("started");
    let args = json!({"program":"node","cols":80,"rows":24,"args":["-e","require('fs').writeFileSync(process.argv[1],'started')",marker.to_string_lossy()]});
    // Negative control: exactly the same executable and arguments really write the marker.
    let allowed = Fixture::new(Some(&manifest(&["node"])), &[]).await;
    let opened = call(&allowed, "cli.pty", args.clone()).await.unwrap();
    call(&allowed, "cli.wait", json!({"process":opened["process"]}))
        .await
        .unwrap();
    assert_eq!(std::fs::read_to_string(&marker).unwrap(), "started");
    close(&allowed).await;
    std::fs::remove_file(&marker).unwrap();
    let mut consent = Consent::undecided();
    consent.set(Right::scoped("cli.exec", "node"), Decision::Substitute);
    let app = Fixture::new(Some(&manifest(&["node"])), &[])
        .await
        .with_consent(consent);
    tokio::time::pause();
    let error = tokio::time::timeout(Duration::from_secs(31), app.call("cli.pty", args))
        .await
        .unwrap()
        .unwrap_err();
    tokio::time::resume();
    assert_eq!(error.code, ErrorCode::Timeout);
    assert!(
        !marker.exists(),
        "substitution started the marker-writing child"
    );
    close(&app).await;
}

#[tokio::test]
async fn terminal_cwd_refusals_and_foreign_resize_do_not_start_a_child() {
    use alef_core::security::consent::{Consent, Decision, Right};
    let dir = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let scope = format!("{}/**", dir.path().to_string_lossy().replace('\\', "/"));
    let mut consent = Consent::undecided();
    consent.set(Right::scoped("cli.exec", "node"), Decision::Allow);
    consent.set(Right::scoped("fs.read", &scope), Decision::Substitute);
    let app = Fixture::new(Some(&super::manifest_with_read(&["node"], &scope)), &[])
        .await
        .with_consent(consent);
    assert_eq!(
        refused(
            &app,
            "cli.pty",
            json!({"program":"node","cols":80,"rows":24,"cwd":dir.path().to_string_lossy()})
        )
        .await,
        ErrorCode::PermissionDenied
    );
    let missing = dir.path().join("missing");
    assert_eq!(
        refused(
            &app,
            "cli.pty",
            json!({"program":"node","cols":80,"rows":24,"cwd":missing.to_string_lossy()})
        )
        .await,
        ErrorCode::InvalidArgument
    );
    assert_eq!(
        call(
            &app,
            "cli.resize",
            json!({"process":999,"cols":80,"rows":24})
        )
        .await
        .unwrap_err()
        .code,
        ErrorCode::NotFound
    );
    close(&app).await;
}

#[tokio::test]
async fn arguments_reach_the_terminal_child_exactly_as_given() {
    let app = Fixture::new(Some(&manifest(&["node"])), &[]).await;
    let mut args = vec![
        "-e".to_owned(),
        "process.stdout.write('RESULT:'+JSON.stringify(process.argv.slice(1))+'\\n')".to_owned(),
    ];
    args.extend(
        super::spawn::TRICKY_ARGUMENTS
            .iter()
            .map(|argument| (*argument).to_owned()),
    );
    let opened = call(
        &app,
        "cli.pty",
        json!({ "program": "node", "cols": 1000, "rows": 1, "args": args }),
    )
    .await
    .unwrap();
    let mut pipe = Pipe::open(&app, opened["output"].as_u64().unwrap());
    let bytes = tokio::time::timeout(Duration::from_secs(60), pipe.until_end())
        .await
        .unwrap()
        .unwrap();
    let expected = format!(
        "RESULT:{}",
        serde_json::to_string(&super::spawn::TRICKY_ARGUMENTS).unwrap()
    );
    let text = String::from_utf8_lossy(&bytes);
    assert!(text.contains(&expected), "{text}");
    close(&app).await;
}

/// A child whose output nobody reads is held back by the credit window, not spooled without bound.
#[tokio::test]
async fn a_terminal_nobody_reads_holds_its_child_back() {
    use alef_core::protocol::credit::DEFAULT_STREAM_WINDOW;
    let dir = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let progress = dir.path().join("progress");
    let app = Fixture::new(Some(&manifest(&["node"])), &[]).await;
    let script = "const fs=require('fs');const fd=fs.openSync(process.env.PROGRESS,'w');let n=0;const chunk='x'.repeat(32768);const pump=()=>process.stdout.write(chunk,()=>{n+=32768;fs.writeSync(fd,String(n).padStart(12,'0'),0);pump()});pump()";
    call(&app,"cli.pty",json!({"program":"node","cols":80,"rows":24,"args":["-e",script],"env":[["PROGRESS",progress.to_string_lossy()]]})).await.unwrap();
    let written = || {
        std::fs::read_to_string(&progress)
            .ok()
            .and_then(|text| text.trim().parse::<usize>().ok())
            .unwrap_or(0)
    };
    // The window and the 256 KiB the pump keeps beyond it, and room for what the kernel buffers.
    let limit = DEFAULT_STREAM_WINDOW + 2 * 1024 * 1024;
    let outcome = tokio::time::timeout(Duration::from_secs(60), async {
        let (mut last, mut steady) = (usize::MAX, 0);
        loop {
            tokio::time::sleep(Duration::from_millis(250)).await;
            let now = written();
            steady = if now == last { steady + 1 } else { 0 };
            if now > limit || (now >= DEFAULT_STREAM_WINDOW / 2 && steady >= 8) {
                return now;
            }
            last = now;
        }
    })
    .await;
    close(&app).await;
    let written = outcome.expect("the child is held back or runs away");
    assert!(
        written <= limit,
        "the child wrote {written} bytes nobody read"
    );
}

/// The output of a finished terminal waits for the page, and a terminal that was waited for is no more resized.
#[tokio::test]
async fn a_terminal_waited_for_keeps_its_output_and_is_no_more_resized() {
    use alef_core::protocol::credit::DEFAULT_STREAM_WINDOW;
    let app = Fixture::new(Some(&manifest(&["node"])), &[]).await;
    let count = DEFAULT_STREAM_WINDOW + 64 * 1024;
    let script = format!("process.stdout.write('o'.repeat({count}),()=>process.exit(0))");
    let opened = call(
        &app,
        "cli.pty",
        json!({"program":"node","cols":1000,"rows":1000,"args":["-e",script]}),
    )
    .await
    .unwrap();
    // Everything is observed first and the session closed before any assertion: a failure must not leave a pseudo console open.
    let waited = call(&app, "cli.wait", json!({"process":opened["process"]})).await;
    let retained = !app.session().resources().is_empty();
    let resized = call(
        &app,
        "cli.resize",
        json!({"process":opened["process"],"cols":90,"rows":30}),
    )
    .await;
    let mut pipe = Pipe::open(&app, opened["output"].as_u64().unwrap());
    let output = tokio::time::timeout(Duration::from_secs(20), pipe.until_end()).await;
    let gone = tokio::time::timeout(Duration::from_secs(20), async {
        while !app.session().resources().is_empty() {
            tokio::time::sleep(Duration::from_millis(25)).await;
        }
    })
    .await;
    close(&app).await;
    assert_eq!(waited.unwrap()["code"], 0);
    assert!(
        retained,
        "the output of a finished terminal is still unread"
    );
    assert_eq!(resized.unwrap_err().code, ErrorCode::NotFound);
    let bytes = output.expect("the output ends").unwrap();
    assert!(bytes.len() >= count, "{} bytes of {count}", bytes.len());
    gone.expect("the finished terminal is no more a resource");
}

/// A script of cmd cannot be run in a terminal: its arguments would be parsed by cmd, not by the program.
#[cfg(windows)]
#[tokio::test]
async fn a_terminal_does_not_run_a_script_of_cmd() {
    let dir = tempfile::tempdir_in(std::env::current_dir().unwrap()).unwrap();
    let app = Fixture::new(Some(&manifest(&["*"])), &[]).await;
    for name in ["tool.cmd", "tool.BAT"] {
        let script = dir.path().join(name);
        std::fs::write(&script, "@echo off\r\n").unwrap();
        assert_eq!(
            refused(
                &app,
                "cli.pty",
                json!({"program":script.to_string_lossy(),"cols":80,"rows":24})
            )
            .await,
            ErrorCode::InvalidArgument
        );
    }
    close(&app).await;
}

/// A Unix terminal reads the end of the input stream as the end of file; ConPTY has no such thing.
#[cfg(unix)]
#[tokio::test]
async fn the_end_of_the_input_stream_is_the_end_of_file_of_the_terminal() {
    let app = Fixture::new(Some(&manifest(&["node"])), &[]).await;
    let opened = call(&app,"cli.pty",json!({"program":"node","cols":80,"rows":24,"args":["-e","process.stdin.on('data',()=>{});process.stdin.on('end',()=>process.exit(0));setTimeout(()=>process.exit(7),30000)"]})).await.unwrap();
    app.session()
        .streams()
        .incoming_writer(StreamId(opened["input"].as_u64().unwrap()))
        .unwrap()
        .end();
    let waited = tokio::time::timeout(
        Duration::from_secs(20),
        call(&app, "cli.wait", json!({"process":opened["process"]})),
    )
    .await;
    close(&app).await;
    assert_eq!(
        waited
            .expect("the child sees the end of its input")
            .unwrap()["code"],
        0
    );
}
