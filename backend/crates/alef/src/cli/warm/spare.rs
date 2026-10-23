// SPDX-License-Identifier: MIT OR Apache-2.0
//! `alef warm-spare` (docs/stages/m11-warm-start.md, «Архитектура реализации»):
//! the internal spare process the broker spawns. Windowed: the main thread
//! does nothing before [`alef_runtime::run_warm`] — the event loop, the
//! hidden window and the GL context are prepared there; the assignment
//! arrives on the worker thread, applies the client's cwd and environment
//! exactly, re-runs the whole cold launch path and hands `bridge` +
//! `options` to the event loop instead of the last cold step. After the
//! event loop the finish runs on the main thread, with the worker's tokio
//! runtime kept alive. Any refusal before an irreversible action is a
//! `decline` and a quiet exit.

use std::error::Error;
use std::ffi::OsString;
use std::io::{self, ErrorKind};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use alef_broker::ipc::endpoint::Endpoint;
use alef_broker::peer::spare::{Assignment, Spare};
use alef_broker::protocol::message::Launch as BrokerLaunch;
use alef_broker::protocol::warm::SlotKind as WireKind;
use alef_launch::args::{parse_args, Command, SpareKind};
use alef_launch::consent::Asker;
use alef_runtime::{run_warm, WarmAssignment, WarmPrepare};

use super::endpoint::endpoint_from_arg;
use super::env::{self, EnvSource as _};
use crate::cli::app_run::{self, AppFinish, PreparedApp, ResolvedRun};
use crate::cli::dispatch::resolve_run;
use crate::cli::failure::Failure;

/// How long the spare waits for the single assignment of its life.
const ASSIGN_TIMEOUT: Duration = Duration::from_secs(30);

/// What the assignment leaves for the finish on the main thread.
enum SpareOutcome {
    /// The application was prepared and ran in the event loop.
    App(AppFinish),
    /// The launch asked to exit before anything ran (its own `--help`).
    Exit(u8),
}

type OutcomeSlot = Arc<Mutex<Option<SpareOutcome>>>;
type RuntimeSlot = Arc<Mutex<Option<Arc<tokio::runtime::Runtime>>>>;

pub(crate) async fn run_spare(
    kind: SpareKind,
    spare_id: u64,
    endpoint_arg: OsString,
) -> Result<u8, Failure> {
    let ep = endpoint_from_arg(&endpoint_arg).ok_or_else(|| {
        Failure::Usage("warm-spare: the entry point is not a broker endpoint".to_owned())
    })?;
    let wire_kind = match kind {
        SpareKind::Windowed => WireKind::Windowed,
        SpareKind::Windowless => WireKind::Windowless,
    };
    if kind == SpareKind::Windowless {
        // The slot is off in this version (the measured gain was ≈ 5–16 ms);
        // the kind stays parseable for the future.
        if let Ok(mut spare) = Spare::connect(&ep, wire_kind, spare_id) {
            let _ = spare.decline("windowless spares are disabled in this version");
        }
        return Ok(0);
    }
    let shared: OutcomeSlot = Arc::new(Mutex::new(None));
    let runtime_slot: RuntimeSlot = Arc::new(Mutex::new(None));
    let prepare = WarmPrepare {
        assignment: Box::new({
            let shared = Arc::clone(&shared);
            let runtime_slot = Arc::clone(&runtime_slot);
            move || assignment(wire_kind, spare_id, ep, shared, runtime_slot)
        }),
    };
    let loop_result = run_warm(prepare);
    let outcome = shared.lock().expect("spare outcome").take();
    // The worker's runtime must live to the end: the finish (bridge
    // shutdown, the signal task) uses its handle.
    let worker_runtime = runtime_slot.lock().expect("spare runtime").take();
    let result = match outcome {
        Some(SpareOutcome::App(mut finish)) => {
            // The loop hands the bridge back: it is shut down as on the cold path.
            let result = loop_result.map(|bridge| finish.bridge = bridge);
            app_run::finish_app(finish, result).await
        }
        Some(SpareOutcome::Exit(code)) => Ok(code),
        // Declined or shut down before anything ran: a quiet exit.
        None => Ok(0),
    };
    // A runtime must not be dropped inside async code (tokio panics): a plain thread drops it,
    // unjoined (its blocking tasks must not hold the exit).
    if let Some(runtime) = worker_runtime {
        std::thread::spawn(move || drop(runtime));
    }
    result
}

/// Why the assignment did not become an application.
enum AssignFailure {
    /// Before any irreversible action: the broker retires this spare.
    Decline(String),
    /// The launch asked to exit before anything ran (its own `--help`).
    Exit(u8),
}

fn assign_outcome(
    result: Result<WarmAssignment, AssignFailure>,
    spare: &mut Spare,
    shared: &OutcomeSlot,
) -> Result<WarmAssignment, Box<dyn Error + Send>> {
    match result {
        // Every refusal (the package, the trust, the consent) is behind us: from here the
        // launch is the broker's to accept, the client is told so only now.
        Ok(assignment) => match spare.started() {
            Ok(()) => Ok(assignment),
            Err(_) => Err(quiet_exit()),
        },
        Err(AssignFailure::Decline(reason)) => {
            let _ = spare.decline(&reason);
            Err(quiet_exit())
        }
        Err(AssignFailure::Exit(code)) => {
            *shared.lock().expect("spare outcome") = Some(SpareOutcome::Exit(code));
            Err(quiet_exit())
        }
    }
}

fn quiet_exit() -> Box<dyn Error + Send> {
    Box::new(io::Error::new(
        ErrorKind::ConnectionAborted,
        "the spare exits",
    ))
}

/// The worker-thread task of [`run_warm`]: connect, wait, then run the whole
/// launch path of the application up to (but not including) the event loop.
fn assignment(
    kind: WireKind,
    spare_id: u64,
    ep: Endpoint,
    shared: OutcomeSlot,
    runtime_slot: RuntimeSlot,
) -> Result<WarmAssignment, Box<dyn Error + Send>> {
    let mut spare = match Spare::connect(&ep, kind, spare_id) {
        Ok(spare) => spare,
        // Unknown or retired spare, or no broker: exit quietly.
        Err(_) => return Err(quiet_exit()),
    };
    if spare.ready().is_err() {
        return Err(quiet_exit());
    }
    let launch = match spare.wait_assignment(ASSIGN_TIMEOUT) {
        Ok(Assignment::Launch(launch)) => launch,
        // Shutdown, timeout or a lost connection: the process just ends.
        Err(_) => return Err(quiet_exit()),
    };
    let result = run_assigned(launch, &shared, &runtime_slot);
    assign_outcome(result, &mut spare, &shared)
}

/// The launch path of the application, up to the event loop.
fn run_assigned(
    launch: BrokerLaunch,
    shared: &OutcomeSlot,
    runtime_slot: &RuntimeSlot,
) -> Result<WarmAssignment, AssignFailure> {
    // The client's argv replayed exactly: the `run` form is the only one the
    // client warms, so it is prepended for the parser.
    let mut argv = vec![OsString::from("run")];
    for arg in &launch.args {
        argv.push(
            arg.clone()
                .into_os_string()
                .map_err(|_| AssignFailure::Decline("an argument from another platform".into()))?,
        );
    }
    let cli_launch = match parse_args(argv.clone())
        .map_err(|error| AssignFailure::Decline(format!("warm-spare: {error}")))?
    {
        Command::Run(launch) => launch,
        _ => {
            return Err(AssignFailure::Decline(
                "the assignment is not a run".to_owned(),
            ))
        }
    };
    // The client's context exactly, before anything reads it: the cwd and
    // the full environment (absent variables are removed).
    env::set_cwd(launch.cwd.as_ref()).map_err(AssignFailure::Decline)?;
    let current = env::ProcessEnv.vars();
    env::apply(&current, &launch.env, &mut env::ProcessEnvTarget)
        .map_err(AssignFailure::Decline)?;
    // An interactive permission prompt cannot happen here: no window exists
    // on this thread. The client runs cold and asks in its own process.
    if matches!(
        Asker::from_environment(cli_launch.grant, cli_launch.no_prompt)
            .map_err(AssignFailure::Decline)?,
        Asker::Window { .. }
    ) {
        return Err(AssignFailure::Decline(
            "an interactive permission decision may be required".to_owned(),
        ));
    }
    // This thread's runtime outlives the assignment: the finish on the main
    // thread uses its handle (Bridge and the signal task).
    let runtime = Arc::new(
        tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .map_err(|error| AssignFailure::Decline(format!("the async runtime: {error}")))?,
    );
    *runtime_slot.lock().expect("spare runtime") = Some(Arc::clone(&runtime));
    let prepared = runtime.block_on(async {
        let run = match resolve_run(argv, cli_launch).await {
            Ok(ResolvedRun::Run(run)) => run,
            Ok(ResolvedRun::Exit(code)) => return Err(AssignFailure::Exit(code)),
            Err(failure) => return Err(AssignFailure::Decline(failure_message(failure))),
        };
        if let Some(reason) = run.warm_decline_reason() {
            return Err(AssignFailure::Decline(reason));
        }
        match app_run::prepare_app(run).await {
            Ok(Ok(prepared)) => Ok(prepared),
            Ok(Err(code)) => Err(AssignFailure::Exit(code)),
            Err(failure) => Err(AssignFailure::Decline(failure_message(failure))),
        }
    });
    let prepared = prepared?;
    let PreparedApp {
        bridge,
        options,
        console,
        signal_task,
        handle,
        process_args,
        ..
    } = prepared;
    *shared.lock().expect("spare outcome") = Some(SpareOutcome::App(AppFinish {
        // The bridge moves into the event loop with the assignment and comes back
        // from `run_warm` for the finish.
        bridge: None,
        console,
        signal_task,
        handle,
        process_args,
    }));
    Ok(WarmAssignment { bridge, options })
}

/// The one message of a `Failure` (the category prefixes keep their meaning).
fn failure_message(failure: Failure) -> String {
    match failure {
        Failure::Usage(message) | Failure::Declined(message) | Failure::Runtime(message) => {
            format!("warm-spare: {message}")
        }
    }
}
