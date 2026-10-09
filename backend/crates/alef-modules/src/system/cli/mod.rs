// SPDX-License-Identifier: MIT OR Apache-2.0
//! `cli`: running programs as `permissions.cli.exec` lets them be. `cli.exec` runs one line and
//! gives back what it wrote; `cli.spawn` wires the pipes of a process to streams of the page;
//! `cli.wait` and `cli.kill` watch it and take it down. A process is a whole tree: whatever the
//! child starts goes with it, and a right the user substituted starts nothing at all.
//! `cli.run` and `cli.start` do the same for a command the manifest declares, by name.
//! `sidecar:<name>` is a program of the application (`bin/<name>`), never one found on `PATH`.
mod declared;
pub(crate) mod exec;
#[cfg(windows)]
mod native;
pub(crate) mod pty;
pub(crate) mod spawn;
pub(crate) mod tree;

use declared::{commands, sidecar};
use std::path::{Path, PathBuf};
use std::time::Duration;

use alef_core::{
    ids::ResourceId,
    registry::{command::Reply, context::CallContext, dispatch::Registry},
    security::{consent::Decision, permissions::Permission},
    AlefError, ErrorCode,
};
use serde::Deserialize;
use serde_json::{json, Value};

use crate::json;

/// How long a call the user substituted hangs when it names no timeout.
const HANG: Duration = Duration::from_secs(30);

/// What the user substituted for a program: nothing starts, and the call hangs until its time is up.
pub(crate) async fn dead(limit: Option<Duration>) -> AlefError {
    tokio::time::sleep(limit.unwrap_or(HANG)).await;
    AlefError::new(ErrorCode::Timeout, "the process did not start in time")
}

/// What `cli.exec` and `cli.run` answer with.
fn run_reply(
    out: &[u8],
    err: &[u8],
    code: Option<i32>,
    signal: Option<String>,
) -> Result<Reply, AlefError> {
    json(&json!({
        "code": code,
        "signal": signal,
        "stdout": String::from_utf8_lossy(out),
        "stderr": String::from_utf8_lossy(err),
    }))
}

fn invalid(message: &str) -> AlefError {
    AlefError::new(ErrorCode::InvalidArgument, message)
}

/// Whether the right is `*`: the only way an operator may reach a shell or an environment variable
/// may carry the search path.
pub(crate) fn wildcard(ctx: &CallContext) -> bool {
    ctx.permissions
        .check(Permission::CliExec, Some("*"), &ctx.grants())
        .is_ok_and(|decision| decision == Decision::Allow)
}

/// The first word of a command line: the target of the permission. A line without one asks for
/// nothing, and a nothing is refused.
pub(crate) fn first_token(line: &str) -> Option<String> {
    let mut words = exec::split(line).or_else(|| {
        // Shell syntax belongs to the shell; authorization still checks its first word.
        line.split_whitespace()
            .next()
            .map(|word| vec![word.to_owned()])
    })?;
    if words.is_empty() {
        None
    } else {
        Some(words.remove(0))
    }
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ExecArgs {
    command_line: String,
    #[serde(default)]
    shell: Option<exec::ShellArg>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    env: Option<Vec<(String, String)>>,
    #[serde(default)]
    timeout_ms: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct SpawnArgs {
    program: String,
    #[serde(default)]
    args: Option<Vec<String>>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    env: Option<Vec<(String, String)>>,
    #[serde(default)]
    stdin: Option<spawn::PipeArg>,
    #[serde(default)]
    stdout: Option<spawn::PipeArg>,
    #[serde(default)]
    stderr: Option<spawn::PipeArg>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WaitArgs {
    process: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "SCREAMING_SNAKE_CASE")]
enum KillSignal {
    Sigterm,
    Sigkill,
    Sigint,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct KillArgs {
    process: u64,
    #[serde(default)]
    signal: Option<KillSignal>,
}

/// The working directory of a process: an existing folder the `fs.read` right reaches, or the one
/// the runtime runs in.
fn cwd_of(ctx: &CallContext, cwd: &Option<String>) -> Result<PathBuf, AlefError> {
    use alef_core::security::permissions::{Permission, Reach};
    match cwd {
        Some(cwd) => {
            if !Path::new(cwd).is_dir() {
                return Err(invalid("cwd is not an existing directory"));
            }
            let authorized = ctx.permissions.authorize_at(
                Permission::FsRead,
                Some(cwd),
                &ctx.grants(),
                Reach::Through,
            )?;
            if authorized.decision == Decision::Substitute {
                return Err(AlefError::new(
                    ErrorCode::PermissionDenied,
                    "substituted cwd cannot be used by a process",
                ));
            }
            Ok(authorized.path)
        }
        None => std::env::current_dir()
            .map_err(|error| AlefError::new(ErrorCode::Internal, format!("cwd: {error}"))),
    }
}

/// Takes a tree down the way the caller asked for; on Windows only the whole tree dies at once.
fn apply_signal(process: &spawn::Process, signal: KillSignal) {
    #[cfg(unix)]
    match signal {
        KillSignal::Sigkill => process.shared.killer.kill(),
        KillSignal::Sigterm => process.shared.killer.signal(libc::SIGTERM),
        KillSignal::Sigint => process.shared.killer.signal(libc::SIGINT),
    }
    #[cfg(windows)]
    {
        let _ = signal;
        process.shared.killer.kill();
    }
}

pub(crate) fn register(
    registry: &mut Registry,
    context: &crate::ModuleContext,
) -> Result<(), AlefError> {
    let app = context.paths.app.clone();
    let exec_app = app.clone();
    let spawn_app = app.clone();
    pty::register(registry)?;
    registry
        .command::<ExecArgs>("cli.exec")?
        .permission(Permission::CliExec, |args| first_token(&args.command_line))
        .substitutes()
        .handler(move |ctx, args| {
            let app = exec_app.clone();
            async move {
                use alef_core::security::permissions::Permission;
                let limit = args.timeout_ms.map(Duration::from_millis);
                if ctx.decision() == Decision::Substitute {
                    return Err(dead(limit).await);
                }
                let env = spawn::check_env(args.env, wildcard(&ctx))?;
                let (program_text, arguments) = match exec::shell_of(&args.shell)? {
                    Some(shell) => {
                        let shell_decision = ctx.permissions.check(
                            Permission::CliExec,
                            Some(shell.name()),
                            &ctx.grants(),
                        )?;
                        if !wildcard(&ctx) && exec::needs_wildcard(&args.command_line) {
                            return Err(invalid("operators need permissions.cli.exec: [*]"));
                        }
                        if shell_decision == Decision::Substitute {
                            return Err(dead(limit).await);
                        }
                        sidecar::refuse_in_shell(first_token(&args.command_line).as_deref())?;
                        let (program, arguments) = shell.command(&args.command_line);
                        (program.to_owned(), arguments)
                    }
                    None => {
                        let mut words = exec::split(&args.command_line)
                            .ok_or_else(|| invalid("the command line has an unterminated quote"))?;
                        if words.is_empty() {
                            return Err(invalid("the command line is empty"));
                        }
                        let program = words.remove(0);
                        (program, words)
                    }
                };
                let cwd = cwd_of(&ctx, &args.cwd)?;
                let program = sidecar::program(&app, &program_text)?;
                let body = ctx.body().cloned();
                let (out, err, code, signal) = exec::run(exec::Spec {
                    program,
                    args: arguments,
                    cwd,
                    env,
                    body,
                    limit,
                })
                .await?;
                run_reply(&out, &err, code, signal)
            }
        })?;

    registry
        .command::<SpawnArgs>("cli.spawn")?
        .permission(Permission::CliExec, |args| Some(args.program.clone()))
        .substitutes()
        .handler(move |ctx, args| {
            let app = spawn_app.clone();
            async move {
                if ctx.decision() == Decision::Substitute {
                    return Err(dead(None).await);
                }
                let env = spawn::check_env(args.env, wildcard(&ctx))?;
                let cwd = cwd_of(&ctx, &args.cwd)?;
                let program = sidecar::program(&app, &args.program)?;
                let reply = spawn::spawn_command(
                    &ctx.session,
                    program,
                    args.args.unwrap_or_default(),
                    cwd,
                    env,
                    spawn::pipes_of(args.stdin, args.stdout, args.stderr),
                )
                .await?;
                json(&reply)
            }
        })?;

    commands::register(registry, app)?;

    registry
        .command::<WaitArgs>("cli.wait")?
        .handler(|ctx, args| async move {
            let shared = ctx
                .resources()
                .with_as::<spawn::Process, _>(ResourceId(args.process), |process| {
                    process.shared.clone()
                })?;
            let (code, signal) = spawn::wait(shared).await?;
            Ok(Reply::Json(json!({ "code": code, "signal": signal })))
        })?;

    registry
        .command::<KillArgs>("cli.kill")?
        .handler(|ctx, args| async move {
            let signal = args.signal.unwrap_or(KillSignal::Sigkill);
            #[cfg(windows)]
            if signal != KillSignal::Sigkill {
                return Err(invalid("only SIGKILL kills a tree on Windows"));
            }
            ctx.resources().with_as::<spawn::Process, _>(
                ResourceId(args.process),
                |process| {
                    if process
                        .shared
                        .claimed
                        .load(std::sync::atomic::Ordering::Acquire)
                    {
                        return Err(AlefError::new(
                            ErrorCode::NotFound,
                            "process already waited for",
                        ));
                    }
                    apply_signal(process, signal);
                    Ok(())
                },
            )??;
            Ok(Reply::Json(Value::Null))
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_screaming_snake_signal_parses_from_its_uppercase_name() {
        assert_eq!(
            serde_json::from_str::<KillSignal>("\"SIGTERM\"").unwrap(),
            KillSignal::Sigterm
        );
        assert_eq!(
            serde_json::from_str::<KillSignal>("\"SIGKILL\"").unwrap(),
            KillSignal::Sigkill
        );
        assert_eq!(
            serde_json::from_str::<KillSignal>("\"SIGINT\"").unwrap(),
            KillSignal::Sigint
        );
        assert!(serde_json::from_str::<KillSignal>("\"sigkill\"").is_err());
    }
}
