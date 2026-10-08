// SPDX-License-Identifier: MIT OR Apache-2.0
//! Declared commands (`permissions.cli.commands`): the page names a command and fills its
//! parameters; the program and the argument list come from the manifest. `cli.run` is `cli.exec`
//! and `cli.start` is `cli.spawn` for such a command, with the same limits, tree and streams.
use std::{collections::BTreeMap, path::PathBuf, time::Duration};

use alef_core::{
    registry::{context::CallContext, dispatch::Registry},
    security::{
        command::{DeclaredCommand, Program},
        consent::Decision,
        permissions::{refusal, Permission},
    },
    AlefError,
};
use serde::Deserialize;

use super::{cwd_of, dead, exec, json, run_reply, sidecar, spawn};

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct RunArgs {
    name: String,
    #[serde(default)]
    params: Option<BTreeMap<String, String>>,
    #[serde(default)]
    cwd: Option<String>,
    #[serde(default)]
    env: Option<Vec<(String, String)>>,
    #[serde(default)]
    timeout_ms: Option<u64>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct StartArgs {
    name: String,
    #[serde(default)]
    params: Option<BTreeMap<String, String>>,
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

/// What a declared command starts: the program, its arguments, the folder, the environment. A
/// declared command has no `*`, so the variables that steer the search path are always refused.
struct Plan {
    program: PathBuf,
    args: Vec<String>,
    cwd: PathBuf,
    env: Vec<(String, String)>,
}

fn plan(
    ctx: &CallContext,
    app: &std::path::Path,
    name: &str,
    params: Option<BTreeMap<String, String>>,
    cwd: &Option<String>,
    env: Option<Vec<(String, String)>>,
) -> Result<Plan, AlefError> {
    let command: &DeclaredCommand = ctx
        .permissions
        .command(name)
        .ok_or_else(|| refusal(Permission::CliCommand))?;
    let args = command.expand(&params.unwrap_or_default())?;
    let env = spawn::check_env(env, false)?;
    let cwd = cwd_of(ctx, cwd)?;
    let program = match &command.program {
        Program::Sidecar(name) => sidecar::sidecar(app, name)?,
        Program::Plain(text) => sidecar::program(app, text)?,
    };
    Ok(Plan {
        program,
        args,
        cwd,
        env,
    })
}

pub(super) fn register(registry: &mut Registry, app: PathBuf) -> Result<(), AlefError> {
    let run_app = app.clone();
    registry
        .command::<RunArgs>("cli.run")?
        .permission(Permission::CliCommand, |args| Some(args.name.clone()))
        .substitutes()
        .handler(move |ctx, args| {
            let app = run_app.clone();
            async move {
                let limit = args.timeout_ms.map(Duration::from_millis);
                if ctx.decision() == Decision::Substitute {
                    return Err(dead(limit).await);
                }
                let plan = plan(&ctx, &app, &args.name, args.params, &args.cwd, args.env)?;
                let (out, err, code, signal) = exec::run(exec::Spec {
                    program: plan.program,
                    args: plan.args,
                    cwd: plan.cwd,
                    env: plan.env,
                    body: ctx.body().cloned(),
                    limit,
                })
                .await?;
                run_reply(&out, &err, code, signal)
            }
        })?;

    registry
        .command::<StartArgs>("cli.start")?
        .permission(Permission::CliCommand, |args| Some(args.name.clone()))
        .substitutes()
        .handler(move |ctx, args| {
            let app = app.clone();
            async move {
                if ctx.decision() == Decision::Substitute {
                    return Err(dead(None).await);
                }
                let plan = plan(&ctx, &app, &args.name, args.params, &args.cwd, args.env)?;
                let reply = spawn::spawn_command(
                    &ctx.session,
                    plan.program,
                    plan.args,
                    plan.cwd,
                    plan.env,
                    spawn::pipes_of(args.stdin, args.stdout, args.stderr),
                )
                .await?;
                json(&reply)
            }
        })
}
