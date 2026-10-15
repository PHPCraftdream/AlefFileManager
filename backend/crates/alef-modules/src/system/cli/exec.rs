// SPDX-License-Identifier: MIT OR Apache-2.0
//! Running one line and collecting what it wrote: the words of a command line, the shells a line
//! may go through, and the run that reads stdout and stderr under a limit and a timeout.
use std::{path::PathBuf, process::Stdio, time::Duration};

use alef_core::{AlefError, ErrorCode};
use bytes::Bytes;
use serde::Deserialize;
use tokio::{
    io::{AsyncRead, AsyncReadExt, AsyncWriteExt},
    process::Command,
};

use super::{tree::Killer, BlockingExecutor};

/// The most a run collects of one pipe: more than this kills the tree.
const OUTPUT_LIMIT: usize = 16 * 1024 * 1024;

/// How many bytes one read of a pipe takes at the most.
const READ_CHUNK: usize = 64 * 1024;

/// The characters a shell would act on: none of them may reach one without the right `*`.
const OPERATORS: &str = ";&|<>$`(){}\n\r%^!*?[]~#";

/// Splits a command line into words: whitespace separates, `"` groups (the quotes go, adjacent
/// parts join), and nothing else is special. `None` for an unterminated quote.
pub(crate) fn split(line: &str) -> Option<Vec<String>> {
    let mut words = Vec::new();
    let mut word = String::new();
    let mut quoted = false;
    for character in line.chars() {
        match character {
            '"' => quoted = !quoted,
            c if c.is_whitespace() && !quoted => {
                if !word.is_empty() {
                    words.push(std::mem::take(&mut word));
                }
            }
            c => word.push(c),
        }
    }
    if quoted {
        return None;
    }
    if !word.is_empty() {
        words.push(word);
    }
    Some(words)
}

/// Whether the line carries something only a shell would understand, or a quote or a backslash:
/// such a line reaches a shell only with the right `*`.
pub(crate) fn needs_wildcard(line: &str) -> bool {
    line.chars()
        .any(|c| OPERATORS.contains(c) || c == '"' || c == '\\')
}

/// A shell the line may go through.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[allow(clippy::enum_variant_names)] // the variants are the programs, PowerShell among them
pub(crate) enum Shell {
    Cmd,
    Sh,
    PowerShell,
}

/// When defined, `cmd.exe` does not look for the program of a line in the working folder.
const NO_FOLDER_LOOKUP: &str = "NoDefaultCurrentDirectoryInExePath";

impl Shell {
    /// Puts into `env` what the shell needs besides the pairs of the page. `cmd.exe` looks for the
    /// first word of a line in the working folder before the path, and the working folder is the
    /// one the page named: a program planted there would run in place of the one the right names.
    /// The pair goes last, so the page cannot take it back.
    pub(crate) fn guard(&self, env: &mut Vec<(String, String)>) {
        if matches!(self, Self::Cmd) {
            env.retain(|(name, _)| !name.eq_ignore_ascii_case(NO_FOLDER_LOOKUP));
            env.push((NO_FOLDER_LOOKUP.to_owned(), "1".to_owned()));
        }
    }

    /// The name the right is checked against.
    pub(crate) fn name(&self) -> &'static str {
        match self {
            Self::Cmd => "cmd.exe",
            Self::Sh => "sh",
            Self::PowerShell => "powershell",
        }
    }

    /// The program (a bare name for the resolver) and the arguments that carry the line.
    pub(crate) fn command(&self, line: &str) -> (&'static str, Vec<String>) {
        match self {
            Self::Cmd => ("cmd.exe", vec!["/C".to_owned(), line.to_owned()]),
            Self::Sh => ("/bin/sh", vec!["-c".to_owned(), line.to_owned()]),
            Self::PowerShell => (
                "powershell.exe",
                vec![
                    "-NoProfile".to_owned(),
                    "-Command".to_owned(),
                    line.to_owned(),
                ],
            ),
        }
    }
}

/// `shell: true`, `shell: "powershell"`, or the options of a shell.
#[derive(Debug, Deserialize)]
#[serde(untagged)]
pub(crate) enum ShellArg {
    Switch(bool),
    Name(String),
}

/// The shell a call asked for, if any.
pub(crate) fn shell_of(arg: &Option<ShellArg>) -> Result<Option<Shell>, AlefError> {
    match arg {
        None | Some(ShellArg::Switch(false)) => Ok(None),
        Some(ShellArg::Switch(true)) => {
            Ok(Some(if cfg!(windows) { Shell::Cmd } else { Shell::Sh }))
        }
        Some(ShellArg::Name(name)) => match name.as_str() {
            "powershell" if cfg!(windows) => Ok(Some(Shell::PowerShell)),
            "powershell" => Err(AlefError::new(
                ErrorCode::InvalidArgument,
                "powershell is only on Windows",
            )),
            other => Err(AlefError::new(
                ErrorCode::InvalidArgument,
                format!("unknown shell {other}"),
            )),
        },
    }
}

/// Everything a run needs: what to start, where, with which environment, what goes in, by when.
pub(crate) struct Spec {
    pub program: PathBuf,
    pub args: Vec<String>,
    pub cwd: PathBuf,
    pub env: Vec<(String, String)>,
    pub body: Option<Bytes>,
    pub limit: Option<Duration>,
}

/// Reads a pipe under the output limit; a tree that writes more than it may is killed.
async fn read_capped<R: AsyncRead + Unpin>(
    mut reader: R,
    killer: Killer,
    pipe: &str,
) -> Result<Vec<u8>, AlefError> {
    let mut out = Vec::new();
    let mut buffer = vec![0_u8; READ_CHUNK];
    loop {
        let count = reader.read(&mut buffer).await?;
        if count == 0 {
            return Ok(out);
        }
        out.extend_from_slice(&buffer[..count]);
        if out.len() > OUTPUT_LIMIT {
            killer.kill();
            return Err(AlefError::new(
                ErrorCode::InvalidArgument,
                format!("the process wrote more than 16 MiB on {pipe}"),
            ));
        }
    }
}

/// The name of a signal that ended a process, in the words of the platform.
#[cfg(unix)]
pub(crate) fn signal_name(signal: i32) -> String {
    let name = match signal {
        libc::SIGHUP => "SIGHUP",
        libc::SIGINT => "SIGINT",
        libc::SIGQUIT => "SIGQUIT",
        libc::SIGILL => "SIGILL",
        libc::SIGABRT => "SIGABRT",
        libc::SIGKILL => "SIGKILL",
        libc::SIGSEGV => "SIGSEGV",
        libc::SIGTERM => "SIGTERM",
        libc::SIGPIPE => "SIGPIPE",
        libc::SIGALRM => "SIGALRM",
        _ => return format!("signal {signal}"),
    };
    name.to_owned()
}

/// What a run came to: what it wrote on stdout and stderr, how it ended.
/// cancel-safe: yes — dropping the run kills its tree and kill_on_drop reaps the direct child.
/// Normal error/timeout paths additionally await reaping before returning.
pub(crate) async fn run(
    #[cfg_attr(unix, allow(unused_variables))] executor: &BlockingExecutor,
    Spec {
        program,
        args,
        cwd,
        env,
        body,
        limit,
    }: Spec,
) -> Result<(Vec<u8>, Vec<u8>, Option<i32>, Option<String>), AlefError> {
    let mut command = Command::new(&program);
    command
        .args(&args)
        .current_dir(&cwd)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped());
    for (name, value) in &env {
        command.env(name, value);
    }
    command.stdin(if body.is_some() {
        Stdio::piped()
    } else {
        Stdio::null()
    });
    #[cfg(windows)]
    // The body is written after the spawn; a clone rides along only to size the stdin pipe.
    let stdin_body = body.clone();
    #[cfg(unix)]
    let (mut child, killer) = super::tree::spawn(&mut command).await?;
    #[cfg(windows)]
    let (mut child, killer) = executor
        .submit(move || {
            super::native::spawn(
                &program,
                &args,
                &cwd,
                &env,
                super::spawn::Pipes {
                    stdin: stdin_body.is_some(),
                    stdout: true,
                    stderr: true,
                },
            )
        })?
        .result()
        .await??;
    // A dropped run drops every clone of the killer with it: the last one kills the tree.
    let stdin = child.stdin.take();
    let stdout = child.stdout.take().expect("stdout was piped");
    let stderr = child.stderr.take().expect("stderr was piped");
    let work = async {
        let input = async move {
            if let (Some(body), Some(mut stdin)) = (body, stdin) {
                // A child may deliberately close stdin before consuming the body.
                if let Err(error) = stdin.write_all(&body).await {
                    if error.kind() != std::io::ErrorKind::BrokenPipe {
                        return Err(error.into());
                    }
                }
                let _ = stdin.shutdown().await;
            }
            Ok::<_, AlefError>(())
        };
        let exit = async {
            let status = child.wait().await?;
            killer.disarm();
            Ok::<_, AlefError>(status)
        };
        let ((), out, err, status) = tokio::try_join!(
            input,
            read_capped(stdout, killer.clone(), "stdout"),
            read_capped(stderr, killer.clone(), "stderr"),
            exit,
        )?;
        #[cfg(unix)]
        let signal = std::os::unix::process::ExitStatusExt::signal(&status).map(signal_name);
        #[cfg(windows)]
        let signal = None;
        Ok::<_, AlefError>((out, err, status.code(), signal))
    };
    let result = match limit {
        Some(limit) => tokio::time::timeout(limit, work).await.unwrap_or_else(|_| {
            Err(AlefError::new(
                ErrorCode::Timeout,
                "the process did not finish in time",
            ))
        }),
        None => work.await,
    };
    if result.is_err() {
        killer.kill();
        let _ = child.kill().await;
        let _ = child.wait().await;
    }
    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io;

    #[test]
    fn cmd_is_told_not_to_look_in_the_working_folder_and_the_page_cannot_undo_it() {
        let mut env = vec![
            ("A".to_owned(), "b".to_owned()),
            (
                "nodefaultcurrentdirectoryinexepath".to_owned(),
                "0".to_owned(),
            ),
        ];
        Shell::Cmd.guard(&mut env);
        assert_eq!(
            env,
            [
                ("A".to_owned(), "b".to_owned()),
                (NO_FOLDER_LOOKUP.to_owned(), "1".to_owned())
            ]
        );
        for other in [Shell::Sh, Shell::PowerShell] {
            let mut env = vec![("A".to_owned(), "b".to_owned())];
            other.guard(&mut env);
            assert_eq!(env, [("A".to_owned(), "b".to_owned())]);
        }
    }

    #[test]
    fn a_line_splits_into_words_on_whitespace() {
        assert_eq!(
            split("program arg arg"),
            Some(vec![
                "program".to_owned(),
                "arg".to_owned(),
                "arg".to_owned()
            ])
        );
    }

    #[test]
    fn quotes_group_spaces_and_go_away() {
        assert_eq!(
            split(r#"node -e "a b c""#),
            Some(vec!["node".to_owned(), "-e".to_owned(), "a b c".to_owned()])
        );
    }

    #[test]
    fn adjacent_quoted_and_plain_parts_join_into_one_word() {
        assert_eq!(split(r#"ab"c d"e"#), Some(vec!["abc de".to_owned()]));
    }

    #[test]
    fn empty_words_drop_out() {
        assert_eq!(
            split("  a   b  "),
            Some(vec!["a".to_owned(), "b".to_owned()])
        );
        assert_eq!(split(r#"" ""#), Some(vec![" ".to_owned()]));
        assert_eq!(split(""), Some(vec![]));
    }

    #[test]
    fn an_unterminated_quote_is_no_line_at_all() {
        assert_eq!(split("say \"hello"), None);
    }

    #[test]
    fn operator_characters_need_the_right_star() {
        for line in ["a && b", "a; b", "a | b", "a > b", "$(x)", "a\nb"] {
            assert!(needs_wildcard(line), "{line}");
        }
        assert!(!needs_wildcard("program arg arg"));
    }

    #[test]
    fn every_operator_character_needs_the_right_star_on_its_own() {
        for operator in [
            ";", "&", "|", "<", ">", "$", "`", "(", ")", "{", "}", "\n", "\r", "%", "^", "!", "*",
            "?", "[", "]", "~", "#",
        ] {
            assert!(needs_wildcard(&format!("a {operator} b")), "{operator:?}");
        }
    }

    #[test]
    fn a_quote_or_a_backslash_needs_the_right_star_too() {
        assert!(needs_wildcard("say \"hi\""));
        assert!(needs_wildcard(r"a\b"));
    }

    #[test]
    fn the_shell_of_a_switch_depends_on_the_platform() {
        assert_eq!(shell_of(&None).unwrap(), None);
        assert_eq!(shell_of(&Some(ShellArg::Switch(false))).unwrap(), None);
        assert_eq!(
            shell_of(&Some(ShellArg::Switch(true))).unwrap(),
            Some(if cfg!(windows) { Shell::Cmd } else { Shell::Sh })
        );
    }

    #[test]
    fn powershell_is_only_on_windows_and_other_names_are_no_shell() {
        let shell = shell_of(&Some(ShellArg::Name("powershell".to_owned())));
        if cfg!(windows) {
            assert_eq!(shell.unwrap(), Some(Shell::PowerShell));
        } else {
            let error = shell.unwrap_err();
            assert_eq!(error.code, ErrorCode::InvalidArgument);
            assert_eq!(error.message, "powershell is only on Windows");
        }
        assert!(shell_of(&Some(ShellArg::Name("fish".to_owned()))).is_err());
    }

    #[test]
    fn a_shell_line_goes_to_the_program_of_the_platform() {
        let (program, args) = Shell::Cmd.command("echo hi");
        assert_eq!(program, "cmd.exe");
        assert_eq!(args, ["/C", "echo hi"]);
        let (program, args) = Shell::Sh.command("echo hi");
        assert_eq!(program, "/bin/sh");
        assert_eq!(args, ["-c", "echo hi"]);
        let (program, args) = Shell::PowerShell.command("echo hi");
        assert_eq!(program, "powershell.exe");
        assert_eq!(args, ["-NoProfile", "-Command", "echo hi"]);
    }

    #[cfg(windows)]
    fn command(name: &str) -> std::path::PathBuf {
        super::super::tree::resolve(
            name,
            std::env::var_os("PATH").as_deref(),
            std::env::var_os("PATHEXT").as_deref(),
        )
        .expect("command resolves on the search path")
    }

    #[cfg(windows)]
    fn spec(program: std::path::PathBuf, args: &[&str]) -> Spec {
        Spec {
            program,
            args: args.iter().map(|a| (*a).to_owned()).collect(),
            cwd: std::env::temp_dir(),
            env: Vec::new(),
            body: None,
            limit: None,
        }
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn a_saturated_executor_refuses_the_run_with_busy_before_spawning() {
        use std::num::NonZeroUsize;
        use std::sync::mpsc;
        let executor = BlockingExecutor::new(NonZeroUsize::new(1).unwrap());
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = mpsc::sync_channel::<()>(1);
        let blocker = executor
            .submit(move || {
                let _ = started_tx.send(());
                let _ = release_rx.recv_timeout(std::time::Duration::from_secs(5));
            })
            .expect("blocker admitted");
        tokio::time::timeout(std::time::Duration::from_secs(5), started_rx)
            .await
            .expect("blocker started")
            .expect("signal");
        let error = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            run(&executor, spec(command("cmd"), &["/c", "echo", "hi"])),
        )
        .await
        .expect("busy arrives without spawning")
        .expect_err("saturated executor refuses");
        assert_eq!(error.code, ErrorCode::Busy);
        release_tx.send(()).expect("blocker waiting");
        drop(blocker);
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn the_run_yields_to_the_poll_before_its_spawn_is_admitted() {
        use std::future::Future;
        use std::num::NonZeroUsize;
        use std::task::Poll;
        let executor = BlockingExecutor::new(NonZeroUsize::new(1).unwrap());
        let mut fut = Box::pin(run(
            &executor,
            spec(command("cmd"), &["/c", "echo", "pending"]),
        ));
        let poll = fut
            .as_mut()
            .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()));
        assert!(
            matches!(poll, Poll::Pending),
            "the first poll must not spawn"
        );
        drop(fut); // kill_on_drop reaps the tree, if one had been started
    }

    #[cfg(windows)]
    #[test]
    fn a_queued_spawn_dropped_with_its_call_never_starts_the_child() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .max_blocking_threads(1)
            .enable_all()
            .build()
            .expect("test runtime");
        runtime.block_on(async {
            use std::future::Future;
            use std::num::NonZeroUsize;
            use std::sync::mpsc;
            use std::task::Poll;
            // Saturate the blocking pool itself: an admitted job of the executor queues behind it.
            let (block_started_tx, block_started_rx) = tokio::sync::oneshot::channel();
            let (release_tx, release_rx) = mpsc::sync_channel::<()>(1);
            let blocker = tokio::task::spawn_blocking(move || {
                let _ = block_started_tx.send(());
                let _ = release_rx.recv_timeout(std::time::Duration::from_secs(5));
            });
            tokio::time::timeout(std::time::Duration::from_secs(5), block_started_rx)
                .await
                .expect("blocker started")
                .expect("signal");
            let executor = BlockingExecutor::new(NonZeroUsize::new(1).unwrap());
            let target =
                std::env::temp_dir().join(format!("alef-exec-queued-{}", std::process::id()));
            let _ = std::fs::remove_file(&target);
            let spec = Spec {
                program: command("cmd"),
                args: vec![
                    "/c".to_owned(),
                    format!("type nul > \"{}\"", target.display()),
                ],
                cwd: std::env::temp_dir(),
                env: Vec::new(),
                body: None,
                limit: None,
            };
            let mut fut = Box::pin(run(&executor, spec));
            let poll = fut
                .as_mut()
                .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()));
            assert!(matches!(poll, Poll::Pending), "the admitted run queues");
            drop(fut); // the queued spawn must be skipped, never started
            release_tx.send(()).expect("blocker waiting");
            // Draining the queue proves the queued closure was skipped before anything could run.
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                loop {
                    match executor.submit(|| ()) {
                        Ok(job) => break job.result().await,
                        Err(error) if error.code == ErrorCode::Busy => {
                            tokio::task::yield_now().await
                        }
                        Err(error) => panic!("unexpected submit error: {error}"),
                    }
                }
            })
            .await
            .expect("queue drained")
            .expect("sentinel completes");
            assert!(
                !target.exists(),
                "the queued child ran after its call was dropped"
            );
            let _ = std::fs::remove_file(&target);
            drop(blocker);
        });
    }

    #[cfg(windows)]
    #[test]
    fn a_queued_run_that_stays_alive_writes_its_sentinel_once_the_pool_frees() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .max_blocking_threads(1)
            .enable_all()
            .build()
            .expect("test runtime");
        runtime.block_on(async {
            use std::future::Future;
            use std::num::NonZeroUsize;
            use std::sync::mpsc;
            use std::task::Poll;
            // Pool blocker first, as in the dropped-run test.
            let (block_started_tx, block_started_rx) = tokio::sync::oneshot::channel();
            let (release_tx, release_rx) = mpsc::sync_channel::<()>(1);
            let blocker = tokio::task::spawn_blocking(move || {
                let _ = block_started_tx.send(());
                let _ = release_rx.recv_timeout(std::time::Duration::from_secs(5));
            });
            tokio::time::timeout(std::time::Duration::from_secs(5), block_started_rx)
                .await
                .expect("blocker started")
                .expect("signal");
            let executor = BlockingExecutor::new(NonZeroUsize::new(1).unwrap());
            let target =
                std::env::temp_dir().join(format!("alef-exec-queued-alive-{}", std::process::id()));
            let _ = std::fs::remove_file(&target);
            let spec = Spec {
                program: command("cmd"),
                args: vec![
                    "/c".to_owned(),
                    format!("type nul > \"{}\"", target.display()),
                ],
                cwd: std::env::temp_dir(),
                env: Vec::new(),
                body: None,
                limit: None,
            };
            let mut fut = Box::pin(run(&executor, spec));
            let poll = fut
                .as_mut()
                .poll(&mut std::task::Context::from_waker(std::task::Waker::noop()));
            assert!(matches!(poll, Poll::Pending), "the admitted run queues");
            release_tx.send(()).expect("blocker waiting");
            // The queued spawn runs and the run completes: the sentinel appears.
            let (_, _, code, _) = tokio::time::timeout(std::time::Duration::from_secs(10), fut)
                .await
                .expect("run completes")
                .expect("run succeeds");
            assert_eq!(code, Some(0));
            tokio::time::timeout(std::time::Duration::from_secs(5), async {
                while !target.exists() {
                    tokio::time::sleep(std::time::Duration::from_millis(20)).await;
                }
            })
            .await
            .expect("the sentinel appears");
            let _ = std::fs::remove_file(&target);
            drop(blocker);
        });
    }

    #[cfg(windows)]
    #[tokio::test]
    async fn the_run_spawns_on_the_blocking_executor_and_reports_the_child() {
        use std::num::NonZeroUsize;
        let executor = BlockingExecutor::new(NonZeroUsize::new(1).unwrap());
        let (out, err, code, signal) = tokio::time::timeout(
            std::time::Duration::from_secs(10),
            run(&executor, spec(command("cmd"), &["/c", "echo", "ok"])),
        )
        .await
        .expect("run completes")
        .expect("run succeeds");
        assert!(String::from_utf8_lossy(&out).contains("ok"));
        assert!(err.is_empty());
        assert_eq!(code, Some(0));
        assert_eq!(signal, None);
    }

    #[test]
    fn an_io_error_of_the_spawn_keeps_its_code() {
        let error: AlefError = io::Error::from(io::ErrorKind::NotFound).into();
        assert_eq!(error.code, ErrorCode::NotFound);
    }
}
