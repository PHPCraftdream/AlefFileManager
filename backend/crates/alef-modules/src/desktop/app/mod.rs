// SPDX-License-Identifier: MIT OR Apache-2.0
//! `app`: identity, command line, environment, working directory, quit (which documents may
//! veto), relaunch and the single instance.
pub mod args;
pub mod autostart;
pub mod console;
pub mod deeplink;
mod instance;
mod integration;

use std::{
    collections::{BTreeMap, HashMap, HashSet},
    ffi::OsString,
    fmt,
    path::Path,
    sync::{
        atomic::{AtomicU64, Ordering},
        Arc, Mutex, OnceLock, Weak,
    },
    time::Duration,
};

use alef_core::{
    registry::{dispatch::Registry, host::Host},
    security::{consent::Decision, permissions::Permission},
    session::Session,
    AlefError, ErrorCode,
};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::Notify;

use self::instance::{Endpoint, Instance};
pub use self::integration::launch::{LaunchState, LAUNCH_LIMIT};
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

#[derive(Default)]
struct TerminationInner {
    quitting: Arc<Quitting>,
    host: OnceLock<Arc<dyn Host>>,
}

/// How the process is asked to end from outside (a signal): the documents are asked as `app.quit` asks them,
/// and the process quits unless one of them vetoed.
#[derive(Clone, Default)]
pub struct Termination(Arc<TerminationInner>);

impl fmt::Debug for Termination {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Termination").finish_non_exhaustive()
    }
}

impl Termination {
    /// Asks the documents and quits with `code`; `false` when one of them vetoed, or the modules are not
    /// registered yet.
    pub async fn request(&self, code: i32) -> bool {
        let Some(host) = self.0.host.get() else {
            return false;
        };
        if !self.0.quitting.may_quit(host.as_ref()).await {
            return false;
        }
        host.quit(code);
        true
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

/// Starts `program` with `arguments`; the child inherits the environment and the streams. It is
/// no part of the process that starts it: a window child runs in a job or a group that kills what
/// is left of it when it ends, and a restart must not be among those.
fn start_again(program: &Path, arguments: &[OsString]) -> Result<std::process::Child, AlefError> {
    let mut command = std::process::Command::new(program);
    command.args(arguments);
    #[cfg(unix)]
    let spawned = {
        use std::os::unix::process::CommandExt;
        command.process_group(0).spawn()
    };
    #[cfg(windows)]
    let spawned = {
        use std::os::windows::process::CommandExt;
        use windows_sys::Win32::System::Threading::CREATE_BREAKAWAY_FROM_JOB;
        // Refused when the job of this process does not allow the breakaway: then it stays in it.
        command
            .creation_flags(CREATE_BREAKAWAY_FROM_JOB)
            .spawn()
            .or_else(|_| command.creation_flags(0).spawn())
    };
    spawned.map_err(|error| {
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
    integration::launch::register(registry, host.clone(), context)?;
    autostart::register(registry, context)?;
    deeplink::register(registry, context)?;
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
        .substitutes()
        .handler(|ctx, args| async move {
            // A stand-in answers like a variable that is not set: no error, no sign of a stand-in.
            if ctx.decision() == Decision::Substitute {
                return json(&None::<String>);
            }
            json(&value(&args.name))
        })?;

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
                .filter(|name| {
                    matches!(
                        ctx.permissions
                            .check(Permission::AppEnv, Some(name), &ctx.grants()),
                        Ok(Decision::Allow)
                    )
                })
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

    let quitting = context.termination.0.quitting.clone();
    let _ = context.termination.0.host.set(host.clone());
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

    let deep_links = Arc::new(deeplink::DeepLinks::new(context)?);
    let (links, link_host) = (deep_links.clone(), host.clone());
    registry
        .command::<InterceptArgs>("app.openUrlIntercept")?
        .handler(move |ctx, args| {
            let (links, host) = (links.clone(), link_host.clone());
            async move {
                links.intercept(&ctx.session, args.enabled, host.as_ref());
                json(&())
            }
        })?;
    let instance = Arc::new(Instance::new(
        Endpoint::of(context),
        context.args.clone(),
        host.clone(),
        deep_links,
        context.startup_urls.clone(),
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
                    start_again(&std::env::current_exe()?, &arguments).map(drop)
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
        let status = start_again(Path::new(program), &arguments)
            .expect("starts")
            .wait()
            .expect("ends");
        assert!(status.success());
        let error = start_again(Path::new("alef-no-such-program-7f3a"), &[]).unwrap_err();
        assert_eq!(error.code, ErrorCode::NotAvailable);
        assert!(error.message.contains("cannot start a new instance"));
    }

    #[cfg(unix)]
    #[test]
    fn a_program_started_again_leads_a_group_of_its_own() {
        let mut child = start_again(Path::new("sleep"), &[OsString::from("5")]).expect("starts");
        let pid = child.id() as i32;
        // SAFETY: the ids are those of a live child of ours and of this process.
        let (group, own) = unsafe { (libc::getpgid(pid), libc::getpgid(0)) };
        let _ = child.kill();
        let _ = child.wait();
        assert_eq!(group, pid, "it leads a group of its own");
        assert_ne!(group, own);
    }

    // Also the entry point of the helper process, which the test puts in a job and which starts a
    // program again from there.
    #[cfg(windows)]
    #[test]
    fn a_program_started_again_from_a_job_outlives_the_job() {
        use std::os::windows::io::{AsRawHandle, FromRawHandle, OwnedHandle};
        use std::time::Instant;
        use windows_sys::Win32::{
            Foundation::WAIT_TIMEOUT,
            System::{JobObjects::*, Threading::*},
        };

        struct Alive(OwnedHandle);
        impl Drop for Alive {
            fn drop(&mut self) {
                // SAFETY: the handle is that of the program the test started; this is clean-up.
                unsafe {
                    TerminateProcess(self.0.as_raw_handle(), 1);
                }
            }
        }

        let ping = Path::new(&std::env::var_os("SystemRoot").expect("SystemRoot"))
            .join("System32")
            .join("ping.exe");
        if let Some(dir) = std::env::var_os("ALEF_TEST_RESTART_DIR") {
            let dir = std::path::PathBuf::from(dir);
            while !dir.join("go").exists() {
                std::thread::sleep(Duration::from_millis(10));
            }
            let arguments: Vec<OsString> = ["-n", "60", "127.0.0.1"]
                .into_iter()
                .map(OsString::from)
                .collect();
            let child = start_again(&ping, &arguments).expect("starts");
            std::fs::write(dir.join("pid"), child.id().to_string()).expect("writes");
            std::thread::sleep(Duration::from_secs(60));
            return;
        }

        // A job that allows the breakaway lets the restart leave it; one that does not still lets it
        // start, in the job.
        for allowed in [true, false] {
            let dir = tempfile::tempdir().expect("dir");
            // SAFETY: unnamed job with the default security; the limits are an initialised structure.
            let job = unsafe {
                let raw = CreateJobObjectW(std::ptr::null(), std::ptr::null());
                assert!(!raw.is_null(), "job");
                let mut limits: JOBOBJECT_EXTENDED_LIMIT_INFORMATION = std::mem::zeroed();
                limits.BasicLimitInformation.LimitFlags = JOB_OBJECT_LIMIT_KILL_ON_JOB_CLOSE
                    | if allowed {
                        JOB_OBJECT_LIMIT_BREAKAWAY_OK
                    } else {
                        0
                    };
                assert_ne!(
                    SetInformationJobObject(
                        raw,
                        JobObjectExtendedLimitInformation,
                        (&limits as *const JOBOBJECT_EXTENDED_LIMIT_INFORMATION).cast(),
                        std::mem::size_of_val(&limits) as u32,
                    ),
                    0
                );
                OwnedHandle::from_raw_handle(raw)
            };
            let mut helper = std::process::Command::new(std::env::current_exe().expect("exe"))
                .args([
                    "--exact",
                    "desktop::app::tests::a_program_started_again_from_a_job_outlives_the_job",
                ])
                .env("ALEF_TEST_RESTART_DIR", dir.path())
                .stdout(std::process::Stdio::null())
                .stderr(std::process::Stdio::null())
                .spawn()
                .expect("helper");
            // SAFETY: the helper is a live child of ours, the job is ours; the helper waits for `go`.
            assert_ne!(
                unsafe { AssignProcessToJobObject(job.as_raw_handle(), helper.as_raw_handle()) },
                0
            );
            std::fs::write(dir.path().join("go"), "").expect("go");
            let deadline = Instant::now() + Duration::from_secs(30);
            let pid = loop {
                if let Ok(text) = std::fs::read_to_string(dir.path().join("pid")) {
                    if let Ok(pid) = text.parse::<u32>() {
                        break pid;
                    }
                }
                assert!(Instant::now() < deadline, "the helper started nothing");
                std::thread::sleep(Duration::from_millis(20));
            };
            // SAFETY: the pid was just reported by the helper, whose program is alive.
            let again = unsafe { OpenProcess(PROCESS_TERMINATE | PROCESS_SYNCHRONIZE, 0, pid) };
            assert!(!again.is_null(), "open the program");
            // SAFETY: a successful OpenProcess hands over a unique handle.
            let again = Alive(unsafe { OwnedHandle::from_raw_handle(again) });
            drop(job);
            let deadline = Instant::now() + Duration::from_secs(30);
            while helper.try_wait().expect("wait").is_none() {
                assert!(Instant::now() < deadline, "the job did not end the helper");
                std::thread::sleep(Duration::from_millis(20));
            }
            // SAFETY: the handle is owned and live; the wait only probes for half a second.
            let state = unsafe { WaitForSingleObject(again.0.as_raw_handle(), 500) };
            if allowed {
                assert_eq!(
                    state, WAIT_TIMEOUT,
                    "the program started again is still running"
                );
            } else {
                assert_ne!(
                    state, WAIT_TIMEOUT,
                    "it stayed in the job and ended with it"
                );
            }
        }
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
