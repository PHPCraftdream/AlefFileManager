// SPDX-License-Identifier: MIT OR Apache-2.0
//! `app`: identity, command line, environment, working directory, quit and relaunch.
use std::{collections::BTreeMap, ffi::OsString, path::Path, sync::Arc};

use alef_core::{
    registry::{dispatch::Registry, host::Host},
    security::permissions::Permission,
    AlefError, ErrorCode,
};
use serde::Deserialize;
use serde_json::json;

use crate::{json, ModuleContext};

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct QuitArgs {
    #[serde(default)]
    code: Option<i64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct EnvArgs {
    name: String,
}

/// An exit code a process can report on every platform.
fn exit_code(code: Option<i64>) -> Result<i32, AlefError> {
    match code {
        None => Ok(0),
        Some(code @ 0..=255) => Ok(code as i32),
        Some(_) => Err(AlefError::new(
            ErrorCode::InvalidArgument,
            "code must be an integer from 0 to 255",
        )),
    }
}

fn value(name: &str) -> Option<String> {
    std::env::var_os(name).map(|text| text.to_string_lossy().into_owned())
}

/// Starts `program` with `arguments`; the child inherits the environment and the streams.
fn start_again(program: &Path, arguments: &[OsString]) -> Result<(), AlefError> {
    std::process::Command::new(program)
        .args(arguments)
        .spawn()
        .map(drop)
        .map_err(|error| {
            AlefError::new(
                ErrorCode::NotAvailable,
                format!("cannot start a new instance: {error}"),
            )
        })
}

pub(crate) fn register(
    registry: &mut Registry,
    host: Arc<dyn Host>,
    context: &ModuleContext,
) -> Result<(), AlefError> {
    let info = Arc::new(context.app.clone());
    registry
        .command::<()>("app.info")?
        .handler(move |_ctx, ()| {
            let info = info.clone();
            async move { json(&*info) }
        })?;

    let args = Arc::new(context.args.clone());
    registry
        .command::<()>("app.args")?
        .handler(move |_ctx, ()| {
            let args = args.clone();
            async move { json(&*args) }
        })?;

    registry
        .command::<EnvArgs>("app.env")?
        .permission(Permission::AppEnv, |args| Some(args.name.clone()))
        .handler(|_ctx, args| async move { json(&value(&args.name)) })?;

    // Every variable the manifest lists; an empty list is the same refusal as an unlisted name.
    registry
        .command::<()>("app.envAll")?
        .handler(|ctx, ()| async move {
            let names = ctx.permissions.env_names();
            if names.is_empty() {
                return Err(
                    AlefError::new(ErrorCode::PermissionDenied, "permission denied")
                        .with_details(json!({"permission": Permission::AppEnv.name()})),
                );
            }
            let listed: BTreeMap<&str, String> = names
                .into_iter()
                .filter_map(|name| value(name).map(|text| (name, text)))
                .collect();
            json(&listed)
        })?;

    registry
        .command::<()>("app.cwd")?
        .handler(|_ctx, ()| async move {
            let directory = std::env::current_dir()?;
            json(&directory.to_string_lossy())
        })?;

    let quitting = host.clone();
    registry
        .command::<QuitArgs>("app.quit")?
        .handler(move |_ctx, args| {
            let host = quitting.clone();
            async move {
                host.quit(exit_code(args.code)?);
                json(&())
            }
        })?;

    let process_args = Arc::new(context.process_args.clone());
    registry
        .command::<()>("app.relaunch")?
        .handler(move |_ctx, ()| {
            let (host, arguments) = (host.clone(), process_args.clone());
            async move {
                tokio::task::spawn_blocking(move || {
                    start_again(&std::env::current_exe()?, &arguments)
                })
                .await
                .map_err(|error| AlefError::new(ErrorCode::Internal, error.to_string()))??;
                host.quit(0);
                json(&())
            }
        })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_program_is_started_again_and_a_missing_one_is_reported() {
        let (program, arguments): (&str, &[&str]) = if cfg!(windows) {
            ("cmd", &["/C", "exit 0"])
        } else {
            ("true", &[])
        };
        let arguments: Vec<OsString> = arguments.iter().map(OsString::from).collect();
        start_again(Path::new(program), &arguments).expect("starts");
        let error = start_again(Path::new("alef-no-such-program-7f3a"), &[]).unwrap_err();
        assert_eq!(error.code, ErrorCode::NotAvailable);
        assert!(error.message.contains("cannot start a new instance"));
    }

    #[test]
    fn exit_codes_are_bytes() {
        assert_eq!(exit_code(None).unwrap(), 0);
        assert_eq!(exit_code(Some(0)).unwrap(), 0);
        assert_eq!(exit_code(Some(255)).unwrap(), 255);
        for bad in [-1, 256, i64::MAX, i64::MIN] {
            assert_eq!(
                exit_code(Some(bad)).unwrap_err().code,
                ErrorCode::InvalidArgument
            );
        }
    }
}
