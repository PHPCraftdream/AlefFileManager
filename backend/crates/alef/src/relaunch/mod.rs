// SPDX-License-Identifier: MIT OR Apache-2.0
//! A window child inherits the original environment and standard streams. No shell or UTF-8
//! conversion is involved. Platform guards are installed before any child application code runs.
use std::{
    ffi::OsString,
    io,
    path::Path,
    process::ExitStatus,
    sync::atomic::{AtomicU32, Ordering},
    time::{Duration, Instant},
};

#[cfg(unix)]
mod unix;
#[cfg(windows)]
mod windows;
#[cfg(unix)]
use unix as platform;
#[cfg(windows)]
use windows as platform;

pub mod runtime;

/// Put the internal switch before URLs and `--`, which end runtime option parsing.
pub fn window_args(arguments: &[OsString]) -> Vec<OsString> {
    // The `run` subcommand must stay first: after the switch it would be the target.
    let (first, rest) = match arguments.split_first() {
        Some((first, rest)) if first.to_str() == Some("run") => (true, rest),
        _ => (false, arguments),
    };
    let mut args = Vec::with_capacity(arguments.len() + 2);
    if first {
        args.push(OsString::from("run"));
    }
    args.extend([OsString::from("--launch-mode"), OsString::from("window")]);
    args.extend(rest.iter().cloned());
    args
}

/// Starts the program detached: it inherits the environment and streams but leaves the job of
/// this process (Windows breakaway) and leads its own process group (Unix), so a restart
/// outlives this one. On Windows a refused breakaway is an error: a child that stayed in a
/// kill-on-close job would die with this process. The std child is dropped, never waited on.
pub fn detached(program: &Path, arguments: &[OsString]) -> io::Result<()> {
    detached_with(program, arguments, &[])
}

/// [`detached`] with extra environment entries on top of the inherited ones.
pub fn detached_with(
    program: &Path,
    arguments: &[OsString],
    extra: &[(String, String)],
) -> io::Result<()> {
    let mut command = std::process::Command::new(program);
    command.args(arguments);
    command.envs(extra.iter().map(|(k, v)| (k.as_str(), v.as_str())));
    #[cfg(unix)]
    {
        use std::os::unix::process::CommandExt;
        command.process_group(0).spawn()?;
    }
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        use windows_sys::Win32::System::Threading::CREATE_BREAKAWAY_FROM_JOB;
        // Refused when the job of this process does not allow the breakaway: the error is kept,
        // a fallback child would stay inside the enclosing kill-on-close job and die with us.
        command.creation_flags(CREATE_BREAKAWAY_FROM_JOB).spawn()?;
    }
    Ok(())
}

/// The pid of the process this restart replaces, taken once at start; the variable is removed so
/// application children never inherit it. Garbage values are taken as "no predecessor" (the
/// bounded wait then does nothing) — fail-open is safe here: the wait is an extra guarantee.
pub const REPLACES_VARIABLE: &str = "ALEF_UPDATE_REPLACES_PID";
static REPLACES_PID: AtomicU32 = AtomicU32::new(0);
pub fn take_replaces_pid() {
    let pid = std::env::var_os(REPLACES_VARIABLE)
        .and_then(|value| value.to_str().and_then(|text| text.parse::<u32>().ok()))
        .unwrap_or(0);
    std::env::remove_var(REPLACES_VARIABLE);
    REPLACES_PID.store(pid, Ordering::Relaxed);
}
pub fn replaces_pid() -> u32 {
    REPLACES_PID.load(Ordering::Relaxed)
}

#[cfg(windows)]
fn process_alive(pid: u32) -> bool {
    use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
    use windows_sys::Win32::{
        Foundation::WAIT_TIMEOUT,
        System::Threading::{OpenProcess, WaitForSingleObject, PROCESS_SYNCHRONIZE},
    };
    // SAFETY: OpenProcess with a read-only right; a null handle means gone or never existed.
    let raw = unsafe { OpenProcess(PROCESS_SYNCHRONIZE, 0, pid) };
    if raw.is_null() {
        // Access denied: the process exists but is not observable.
        return unsafe { windows_sys::Win32::Foundation::GetLastError() }
            == windows_sys::Win32::Foundation::ERROR_ACCESS_DENIED;
    }
    // SAFETY: a successful OpenProcess transfers unique handle ownership; the drop closes it.
    let handle = unsafe { OwnedHandle::from_raw_handle(raw) };
    // SAFETY: uniquely owned handle, zero-timeout read-only probe.
    let alive = unsafe { WaitForSingleObject(handle.as_raw_handle(), 0) } == WAIT_TIMEOUT;
    alive
}

#[cfg(unix)]
fn process_alive(pid: u32) -> bool {
    // A value wider than pid_t cannot name a process.
    let Ok(pid) = i32::try_from(pid) else {
        return false;
    };
    #[cfg(target_os = "linux")]
    if let Ok(stat) = std::fs::read_to_string(format!("/proc/{pid}/stat")) {
        if stat
            .rsplit_once(") ")
            .is_some_and(|(_, tail)| tail.starts_with('Z'))
        {
            return false;
        }
    }
    // SAFETY: signal zero probes existence only, nothing is delivered.
    match unsafe { libc::kill(pid, 0) } {
        0 => true,
        -1 if std::io::Error::last_os_error().raw_os_error() == Some(libc::ESRCH) => false,
        // The process exists but is not ours.
        _ => true,
    }
}

/// Bounded wait until `pid` is gone. `true` when it exited (or was never alive), `false` on timeout.
pub fn wait_for_process_exit(pid: u32, limit: Duration) -> bool {
    let deadline = Instant::now() + limit;
    loop {
        if !process_alive(pid) {
            return true;
        }
        if Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// Relaunch only after bridge shutdown and console drain. Consent, ALEF_HOME, ALEF_APP_HOME and
/// ALEF_E2E remain in the inherited environment; the child reads the already persisted consent.
pub async fn window(arguments: &[OsString]) -> io::Result<u8> {
    let program = std::env::current_exe()?;
    let args = window_args(arguments);
    let mut child = platform::spawn(&program, &args)?;
    Ok(exit_code(child.wait().await?))
}

pub fn exit_code(status: ExitStatus) -> u8 {
    #[cfg(unix)]
    {
        use std::os::unix::process::ExitStatusExt;
        if let Some(signal) = status.signal() {
            return u8::try_from(128 + signal).unwrap_or(1);
        }
    }
    status
        .code()
        .and_then(|code| u8::try_from(code).ok())
        .unwrap_or(1)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::path::PathBuf;

    #[test]
    fn the_replaces_pid_is_fail_open_for_garbage_and_removed_from_the_environment() {
        std::env::set_var(REPLACES_VARIABLE, "abc");
        take_replaces_pid();
        assert_eq!(replaces_pid(), 0);
        assert!(std::env::var_os(REPLACES_VARIABLE).is_none());
        std::env::set_var(REPLACES_VARIABLE, "424242");
        take_replaces_pid();
        assert_eq!(replaces_pid(), 424242);
        assert!(std::env::var_os(REPLACES_VARIABLE).is_none());
        // Reset the process-global so other tests see no predecessor.
        take_replaces_pid();
        assert_eq!(replaces_pid(), 0);
    }

    #[test]
    fn wait_for_process_exit_returns_true_for_a_dead_pid() {
        // A valid positive pid_t that is never a real process.
        assert!(wait_for_process_exit(
            i32::MAX as u32,
            Duration::from_millis(200)
        ));
    }

    #[test]
    fn wait_for_process_exit_times_out_while_the_process_lives() {
        #[cfg(unix)]
        let mut child = std::process::Command::new("sleep")
            .arg("2")
            .spawn()
            .unwrap();
        #[cfg(windows)]
        let mut child = std::process::Command::new("cmd")
            .args(["/C", "ping", "-n", "2", "127.0.0.1"])
            .spawn()
            .unwrap();
        assert!(!wait_for_process_exit(
            child.id(),
            Duration::from_millis(300)
        ));
        let _ = child.kill();
        let _ = child.wait();
    }

    #[test]
    fn the_window_switch_precedes_urls_and_the_separator_and_preserves_original_os_arguments() {
        for tail in [vec!["alef://open/a b"], vec!["--", "a folder"]] {
            let original: Vec<OsString> = ["--app", "a folder"]
                .into_iter()
                .chain(tail)
                .map(OsString::from)
                .collect();
            let crate::args::Command::Run(run) =
                crate::args::parse_args(window_args(&original)).unwrap()
            else {
                panic!("not a launch")
            };
            assert!(run.launch_window);
        }
        let original: Vec<OsString> = [
            "--app",
            "a folder",
            "alef://open/a b",
            "--",
            "--flag",
            "",
            "quoted\"\\",
        ]
        .into_iter()
        .map(OsString::from)
        .collect();
        let result = window_args(&original);
        assert_eq!(
            &result[..2],
            &[OsString::from("--launch-mode"), OsString::from("window")]
        );
        assert_eq!(&result[2..], &original);
        // Parsing sees the internal switch even when a URL or -- follows it.
        let crate::args::Command::Run(run) = crate::args::parse_args(result).unwrap() else {
            panic!("not a launch")
        };
        assert!(run.launch_window);
    }

    #[test]
    fn the_run_subcommand_stays_first_when_a_window_child_is_relaunched() {
        let original: Vec<OsString> = ["run", "pkg.alef", "alef://open"]
            .into_iter()
            .map(OsString::from)
            .collect();
        let result = window_args(&original);
        assert_eq!(
            &result[..],
            &["run", "--launch-mode", "window", "pkg.alef", "alef://open"].map(OsString::from)
        );
        let crate::args::Command::Run(run) = crate::args::parse_args(result).unwrap() else {
            panic!("not a launch")
        };
        assert!(run.launch_window);
        assert_eq!(run.target, PathBuf::from("pkg.alef"));
        let original: Vec<OsString> = ["--app", "pkg.alef"]
            .into_iter()
            .map(OsString::from)
            .collect();
        let result = window_args(&original);
        assert_eq!(
            &result[..],
            &["--launch-mode", "window", "--app", "pkg.alef"].map(OsString::from)
        );
        let crate::args::Command::Run(run) = crate::args::parse_args(result).unwrap() else {
            panic!("not a launch")
        };
        assert!(run.launch_window);
    }

    // This test is also an isolated subprocess entry point: its environment is supplied only
    // by Command::env, never by process-global mutation in the parallel test runner.
    #[tokio::test]
    async fn the_subprocess_host_runs_the_supplied_script_and_exits_with_its_code() {
        let Some(script) = std::env::var_os("ALEF_RELAUNCH_TEST_SCRIPT") else {
            return;
        };
        let node = std::process::Command::new("node")
            .args(["-p", "process.execPath"])
            .output()
            .unwrap();
        assert!(node.status.success());
        let program = std::path::PathBuf::from(String::from_utf8(node.stdout).unwrap().trim());
        let original: Vec<OsString> = [
            "--app",
            "a folder",
            "alef://open/a b",
            "--",
            "--flag",
            "",
            "quoted\"\\",
        ]
        .into_iter()
        .map(OsString::from)
        .collect();
        let mut args = vec![script];
        args.extend(window_args(&original));
        let mut child = platform::spawn(&program, &args).unwrap();
        let status = child.wait().await.unwrap();
        std::process::exit(i32::from(exit_code(status)));
    }

    struct Host(std::process::Child);
    impl Drop for Host {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }
    fn host(script: &std::path::Path) -> std::process::Command {
        let mut command = std::process::Command::new(std::env::current_exe().unwrap());
        command
            .args([
                "--exact",
                "relaunch::tests::the_subprocess_host_runs_the_supplied_script_and_exits_with_its_code",
                "--nocapture",
            ])
            .env("ALEF_RELAUNCH_TEST_SCRIPT", script);
        command
    }
    fn bounded_exit(child: &mut std::process::Child) -> ExitStatus {
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        loop {
            if let Some(status) = child.try_wait().unwrap() {
                return status;
            }
            assert!(std::time::Instant::now() < deadline, "helper did not exit");
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    #[test]
    fn the_actual_subprocess_inherits_streams_and_environment_and_exits_with_code_seven() {
        let temp = tempfile::tempdir().unwrap();
        let script = temp.path().join("exit seven.cjs");
        let expected = serde_json::to_string(&[
            "--launch-mode",
            "window",
            "--app",
            "a folder",
            "alef://open/a b",
            "--",
            "--flag",
            "",
            "quoted\"\\",
        ])
        .unwrap();
        std::fs::write(&script, format!("if(JSON.stringify(process.argv.slice(2)) !== JSON.stringify({expected})) process.exit(8); if(process.env.ALEF_HOME !== 'a home with spaces' || process.env.ALEF_E2E !== '1') process.exit(9); console.log('RELAUNCH_STDOUT'); console.error('RELAUNCH_STDERR'); process.exit(7);")).unwrap();
        let stdout = temp.path().join("stdout");
        let stderr = temp.path().join("stderr");
        let mut child = Host(
            host(&script)
                .env("ALEF_HOME", "a home with spaces")
                .env("ALEF_E2E", "1")
                .stdout(std::fs::File::create(&stdout).unwrap())
                .stderr(std::fs::File::create(&stderr).unwrap())
                .spawn()
                .unwrap(),
        );
        assert_eq!(bounded_exit(&mut child.0).code(), Some(7));
        assert!(std::fs::read_to_string(stdout)
            .unwrap()
            .contains("RELAUNCH_STDOUT"));
        assert!(std::fs::read_to_string(stderr)
            .unwrap()
            .contains("RELAUNCH_STDERR"));
    }

    // Open a Windows handle while the pid is still owned by the live host. It remains the same
    // process after host death, so neither probing nor failure cleanup can hit a recycled pid.
    struct OwnedPid {
        #[cfg(windows)]
        handle: std::os::windows::io::OwnedHandle,
        #[cfg(unix)]
        pid: i32,
    }
    impl OwnedPid {
        fn new(pid: u32) -> Self {
            #[cfg(windows)]
            {
                use std::os::windows::io::FromRawHandle;
                use windows_sys::Win32::System::Threading::*;
                // SAFETY: positive pid just reported by our live subprocess. OpenProcess creates
                // a new owned handle; it pins identity even if the numeric pid is reused later.
                let raw = unsafe { OpenProcess(PROCESS_TERMINATE | PROCESS_SYNCHRONIZE, 0, pid) };
                assert!(!raw.is_null(), "cannot open owned helper pid");
                // SAFETY: successful OpenProcess transfers unique handle ownership.
                Self {
                    handle: unsafe { std::os::windows::io::OwnedHandle::from_raw_handle(raw) },
                }
            }
            #[cfg(unix)]
            {
                Self {
                    pid: i32::try_from(pid).unwrap(),
                }
            }
        }
        fn alive(&self) -> bool {
            #[cfg(windows)]
            {
                use std::os::windows::io::AsRawHandle;
                // SAFETY: uniquely owned handle, zero-timeout read-only probe.
                unsafe {
                    windows_sys::Win32::System::Threading::WaitForSingleObject(
                        self.handle.as_raw_handle(),
                        0,
                    ) == windows_sys::Win32::Foundation::WAIT_TIMEOUT
                }
            }
            #[cfg(unix)]
            {
                #[cfg(target_os = "linux")]
                if let Ok(stat) = std::fs::read_to_string(format!("/proc/{}/stat", self.pid)) {
                    if stat
                        .rsplit_once(") ")
                        .is_some_and(|(_, tail)| tail.starts_with('Z'))
                    {
                        return false;
                    }
                }
                // SAFETY: owned helper pid, signal zero probes only (no delivery).
                unsafe { libc::kill(self.pid, 0) == 0 }
            }
        }
    }
    impl Drop for OwnedPid {
        fn drop(&mut self) {
            if !self.alive() {
                return;
            }
            #[cfg(windows)]
            {
                use std::os::windows::io::AsRawHandle;
                // SAFETY: handle pins our exact helper process, used only for failure cleanup.
                unsafe {
                    windows_sys::Win32::System::Threading::TerminateProcess(
                        self.handle.as_raw_handle(),
                        1,
                    );
                }
            }
            #[cfg(unix)]
            // SAFETY: test-owned pid; only cleanup while helper was observed alive.
            unsafe {
                libc::kill(self.pid, libc::SIGKILL);
            }
        }
    }

    #[test]
    fn parent_death_kills_the_actual_child_within_the_owned_pid_cleanup_bound() {
        let temp = tempfile::tempdir().unwrap();
        let script = temp.path().join("stay alive.cjs");
        let marker = temp.path().join("child.pid");
        std::fs::write(&script, "require('node:fs').writeFileSync(process.env.ALEF_RELAUNCH_PID, String(process.pid)); setInterval(()=>{},1000);").unwrap();
        let mut parent = Host(
            host(&script)
                .env("ALEF_RELAUNCH_PID", &marker)
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .unwrap(),
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        let pid = loop {
            if let Ok(text) = std::fs::read_to_string(&marker) {
                if let Ok(pid) = text.parse::<u32>() {
                    break pid;
                }
            }
            assert!(
                parent.0.try_wait().unwrap().is_none(),
                "host exited before ready"
            );
            assert!(
                std::time::Instant::now() < deadline,
                "child never became ready"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        };
        let owned = OwnedPid::new(pid);
        assert!(owned.alive());
        parent.0.kill().unwrap();
        bounded_exit(&mut parent.0);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while owned.alive() {
            assert!(
                std::time::Instant::now() < deadline,
                "child survived parent death: {pid}"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
    }

    // Also the entry point of the child, which the test starts through the launcher: it starts a
    // program with the breakaway flag, as `app.relaunch` does, and waits to be killed with the job.
    #[cfg(windows)]
    #[test]
    fn a_program_the_child_started_again_outlives_the_parent_and_the_child_does_not() {
        use std::os::windows::process::CommandExt;
        let name = "relaunch::tests::a_program_the_child_started_again_outlives_the_parent_and_the_child_does_not";
        let entry =
            std::env::args().find_map(|arg| arg.strip_prefix("child-entry:").map(String::from));
        if let Some(dir) = entry {
            let dir = std::path::PathBuf::from(dir);
            let mut again = std::process::Command::new("node")
                .args(["-e", "setInterval(()=>{},1000)"])
                .creation_flags(windows_sys::Win32::System::Threading::CREATE_BREAKAWAY_FROM_JOB)
                .spawn()
                .expect("the job allows the breakaway");
            std::fs::write(dir.join("again.pid"), again.id().to_string()).unwrap();
            std::fs::write(dir.join("child.pid"), std::process::id().to_string()).unwrap();
            std::thread::sleep(std::time::Duration::from_secs(60));
            let _ = again.kill();
            let _ = again.wait();
            return;
        }
        let temp = tempfile::tempdir().unwrap();
        let args: Vec<OsString> = [
            "--exact".into(),
            name.into(),
            format!("child-entry:{}", temp.path().display()),
            "--nocapture".into(),
        ]
        .into_iter()
        .map(OsString::from)
        .collect();
        let parent = platform::spawn(&std::env::current_exe().unwrap(), &args).unwrap();
        let pid_of = |file: &str| {
            let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
            loop {
                if let Some(pid) = std::fs::read_to_string(temp.path().join(file))
                    .ok()
                    .and_then(|text| text.parse::<u32>().ok())
                {
                    return pid;
                }
                assert!(std::time::Instant::now() < deadline, "no pid in {file}");
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
        };
        let child = OwnedPid::new(pid_of("child.pid"));
        let again = OwnedPid::new(pid_of("again.pid"));
        drop(parent);
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while child.alive() {
            assert!(
                std::time::Instant::now() < deadline,
                "the child survived its job"
            );
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        std::thread::sleep(std::time::Duration::from_millis(500));
        assert!(
            again.alive(),
            "the program started again was killed with the child"
        );
    }

    #[cfg(unix)]
    #[test]
    fn the_parent_forwards_sigterm_and_maps_the_child_signal_to_exit_code_143() {
        let temp = tempfile::tempdir().unwrap();
        let script = temp.path().join("signal.cjs");
        let marker = temp.path().join("ready");
        std::fs::write(&script, "require('node:fs').writeFileSync(process.env.ALEF_RELAUNCH_PID, String(process.pid)); setInterval(()=>{},1000);").unwrap();
        let mut parent = Host(
            host(&script)
                .env("ALEF_RELAUNCH_PID", &marker)
                .spawn()
                .unwrap(),
        );
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(15);
        while !marker.exists() {
            assert!(std::time::Instant::now() < deadline);
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        // SAFETY: live unreaped owned host child, positive pid; intentional signal test.
        assert_eq!(
            unsafe { libc::kill(parent.0.id() as i32, libc::SIGTERM) },
            0
        );
        assert_eq!(bounded_exit(&mut parent.0).code(), Some(143));
    }

    #[cfg(unix)]
    #[test]
    fn window_arguments_preserve_non_utf8_and_signal_exits_map_to_shell_codes() {
        use std::os::unix::{ffi::OsStringExt, process::ExitStatusExt};
        let arg = OsString::from_vec(vec![0xff, b' ']);
        assert_eq!(window_args(std::slice::from_ref(&arg))[2], arg);
        assert_eq!(exit_code(ExitStatus::from_raw(7 << 8)), 7);
        assert_eq!(exit_code(ExitStatus::from_raw(15)), 143);
        assert_eq!(exit_code(ExitStatus::from_raw(2)), 130);
    }

    #[cfg(windows)]
    #[test]
    fn window_arguments_preserve_unpaired_utf16_and_exit_codes_map_to_bytes() {
        use std::os::windows::{ffi::OsStringExt, process::ExitStatusExt};
        let arg = OsString::from_wide(&[0xd800, 32]);
        assert_eq!(window_args(std::slice::from_ref(&arg))[2], arg);
        assert_eq!(exit_code(ExitStatus::from_raw(7)), 7);
        assert_eq!(exit_code(ExitStatus::from_raw(0)), 0);
        assert_eq!(exit_code(ExitStatus::from_raw(256)), 1);
    }
}
