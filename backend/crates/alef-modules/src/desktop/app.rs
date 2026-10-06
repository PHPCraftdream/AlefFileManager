// SPDX-License-Identifier: MIT OR Apache-2.0
//! `app`: identity, command line, environment, working directory, quit (which documents may
//! veto), relaunch and the single instance.
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    ffi::OsString,
    path::Path,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex, Weak,
    },
    time::Duration,
};

use alef_core::{
    registry::{dispatch::Registry, host::Host},
    security::permissions::Permission,
    session::Session,
    AlefError, ErrorCode,
};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::Notify;

use super::instance::{Endpoint, Instance};
use crate::{json, ModuleContext};

/// The event a document that asked for it gets before the application quits.
const BEFORE_QUIT: &str = "app.before-quit";
/// How long the documents have to answer `app.before-quit`; silence allows the quit.
const QUIT_ANSWER_LIMIT: Duration = Duration::from_secs(3);

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct QuitArgs {
    #[serde(default)]
    code: Option<i64>,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct InterceptArgs {
    enabled: bool,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct AnswerArgs {
    id: u64,
    prevent: bool,
}

/// A document that wants to be asked before the application quits.
struct Interceptor {
    window: u64,
    session: Weak<Session>,
}

impl Interceptor {
    fn alive(&self) -> bool {
        self.session
            .upgrade()
            .is_some_and(|session| session.is_open())
    }
}

/// One ask-before-quitting round: the windows that have not answered yet and whether one vetoed.
struct Round {
    waiting: HashSet<u64>,
    vetoed: bool,
}

struct Pending {
    round: Mutex<Round>,
    answered: Notify,
}

#[derive(Default)]
struct Quitting {
    interceptors: Mutex<Vec<Interceptor>>,
    rounds: Mutex<HashMap<u64, Arc<Pending>>>,
    last: AtomicU64,
}

impl Quitting {
    fn intercept(&self, session: &Arc<Session>, enabled: bool) {
        let mut list = self.interceptors.lock().unwrap_or_else(|e| e.into_inner());
        list.retain(|known| known.alive() && known.window != session.window());
        if enabled {
            list.push(Interceptor {
                window: session.window(),
                session: Arc::downgrade(session),
            });
        }
    }

    /// Asks the documents that want to be asked; `true` when the application may quit.
    async fn may_quit(&self, host: &dyn Host) -> bool {
        let windows: HashSet<u64> = {
            let mut list = self.interceptors.lock().unwrap_or_else(|e| e.into_inner());
            list.retain(Interceptor::alive);
            list.iter().map(|known| known.window).collect()
        };
        if windows.is_empty() {
            return true;
        }
        let id = self.last.fetch_add(1, Ordering::SeqCst) + 1;
        let pending = Arc::new(Pending {
            round: Mutex::new(Round {
                waiting: windows.clone(),
                vetoed: false,
            }),
            answered: Notify::new(),
        });
        self.rounds
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(id, pending.clone());
        for window in windows {
            host.emit(Some(window), BEFORE_QUIT, json!({ "id": id }));
        }
        let waited = tokio::time::timeout(QUIT_ANSWER_LIMIT, async {
            loop {
                let notified = pending.answered.notified();
                {
                    let round = pending.round.lock().unwrap_or_else(|e| e.into_inner());
                    if round.waiting.is_empty() || round.vetoed {
                        return;
                    }
                }
                notified.await;
            }
        })
        .await;
        self.rounds
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .remove(&id);
        let vetoed = pending
            .round
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .vetoed;
        waited.is_err() || !vetoed
    }

    /// A document answers a round; an answer to a round that is over, or from a window that was
    /// not asked, changes nothing.
    fn answer(&self, window: u64, id: u64, prevent: bool) {
        let pending = self
            .rounds
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .get(&id)
            .cloned();
        let Some(pending) = pending else { return };
        let mut round = pending.round.lock().unwrap_or_else(|e| e.into_inner());
        if round.waiting.remove(&window) && prevent {
            round.vetoed = true;
        }
        drop(round);
        pending.answered.notify_waiters();
    }
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

    let quitting = Arc::new(Quitting::default());
    let (quit_host, quit_state) = (host.clone(), quitting.clone());
    registry
        .command::<QuitArgs>("app.quit")?
        .handler(move |_ctx, args| {
            let (host, quitting) = (quit_host.clone(), quit_state.clone());
            async move {
                let code = exit_code(args.code)?;
                if quitting.may_quit(host.as_ref()).await {
                    host.quit(code);
                }
                json(&())
            }
        })?;
    let state = quitting.clone();
    registry
        .command::<InterceptArgs>("app.quitIntercept")?
        .handler(move |ctx, args| {
            let quitting = state.clone();
            async move {
                quitting.intercept(&ctx.session, args.enabled);
                json(&())
            }
        })?;
    let state = quitting.clone();
    registry
        .command::<AnswerArgs>("app.quitAnswer")?
        .handler(move |ctx, args| {
            let quitting = state.clone();
            async move {
                quitting.answer(ctx.session.window(), args.id, args.prevent);
                json(&())
            }
        })?;

    let instance = Arc::new(Instance::new(
        Endpoint::of(context),
        context.args.clone(),
        host.clone(),
    ));
    registry
        .command::<()>("app.requestSingleInstance")?
        .handler(move |_ctx, ()| {
            let instance = instance.clone();
            async move { json(&instance.request().await?) }
        })?;

    let process_args = Arc::new(context.process_args.clone());
    registry
        .command::<()>("app.relaunch")?
        .handler(move |_ctx, ()| {
            let (host, arguments, quitting) =
                (host.clone(), process_args.clone(), quitting.clone());
            async move {
                if !quitting.may_quit(host.as_ref()).await {
                    return json(&());
                }
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
