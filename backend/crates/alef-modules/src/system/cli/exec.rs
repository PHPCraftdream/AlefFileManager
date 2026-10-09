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

use super::tree::Killer;

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

impl Shell {
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
    #[cfg(unix)]
    let (mut child, killer) = super::tree::spawn(&mut command).await?;
    #[cfg(windows)]
    let (mut child, killer) = super::native::spawn(
        &program,
        &args,
        &cwd,
        &env,
        super::spawn::Pipes {
            stdin: body.is_some(),
            stdout: true,
            stderr: true,
        },
    )?;
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

    #[test]
    fn an_io_error_of_the_spawn_keeps_its_code() {
        let error: AlefError = io::Error::from(io::ErrorKind::NotFound).into();
        assert_eq!(error.code, ErrorCode::NotFound);
    }
}
