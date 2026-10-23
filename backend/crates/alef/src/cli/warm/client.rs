// SPDX-License-Identifier: MIT OR Apache-2.0
//! The warm client of `alef run` (docs/stages/m11-warm-start.md,
//! «Архитектура реализации»): before the cold path, one attempt to hand the
//! launch to the broker's spare. A proven decline falls through to the
//! unchanged cold path; an undefined outcome is reported and NEVER retried.

use std::ffi::OsString;
use std::time::{SystemTime, UNIX_EPOCH};

use alef_broker::peer::client::Outcome;
use alef_broker::protocol::message::{Launch as BrokerLaunch, OsBytes};
use alef_launch::args::Launch;
use alef_runtime::mark;

use super::endpoint::{endpoint, endpoint_present};
use super::env;
use crate::cli::failure::Failure;

/// Tries the warm path for `alef run`. `Ok(Some(code))` — the spare ran the
/// application and the process ends with its code; `Ok(None)` — a proven
/// decline (or the warm path does not apply): run the cold path; `Err` — an
/// undefined outcome, never a blind second launch.
pub(crate) fn try_run_warm(
    process_args: &[OsString],
    launch: &Launch,
) -> Result<Option<u8>, Failure> {
    if !should_try(launch, std::env::var("ALEF_WARM").ok().as_deref()) {
        return Ok(None);
    }
    let Some(ep) = endpoint() else {
        return Ok(None);
    };
    // Read-only check: without a live endpoint the cold path starts at once,
    // without creating the token file as a side effect.
    if !endpoint_present(&ep) {
        return Ok(None);
    }
    mark("warm-client-begin");
    let request = build_request(process_args, launch);
    let mut stdout = std::io::stdout();
    let mut stderr = std::io::stderr();
    let outcome = alef_broker::peer::client::try_launch(&ep, &request, &mut stdout, &mut stderr);
    mark("warm-client-end");
    match outcome {
        Outcome::Handled { exit_code } => Ok(Some(u8::try_from(exit_code).unwrap_or(1))),
        Outcome::Cold(reason) => {
            if warm_debug() {
                eprintln!("alef: warm start unavailable: {reason:?}");
            }
            Ok(None)
        }
        Outcome::Ambiguous => Err(Failure::Runtime(
            "WARM_UNDETERMINED: the outcome of the warmed launch is undefined: the request \
             is not repeated to avoid starting the application twice; check the broker with \
             `alef warm status` and start the application again"
                .to_owned(),
        )),
    }
}

fn warm_debug() -> bool {
    std::env::var("ALEF_WARM_DEBUG").is_ok_and(|value| value == "1")
}

/// Whether the warm attempt applies at all: no devtools, no development
/// server, the emergency switch `ALEF_WARM` is not `0`, and the launch came
/// as `alef run <TARGET>` — the only form the spare replays verbatim.
fn should_try(launch: &Launch, warm_var: Option<&str>) -> bool {
    launch.positional
        && launch.devtools_port.is_none()
        && launch.dev_url.is_none()
        && warm_var != Some("0")
}

/// The original argv without the `run` subcommand: exactly what the spare
/// parses back and replays.
fn args_after_subcommand(process_args: &[OsString]) -> &[OsString] {
    match process_args.split_first() {
        Some((first, rest)) if first.to_str() == Some("run") => rest,
        _ => process_args,
    }
}

fn build_request(process_args: &[OsString], launch: &Launch) -> BrokerLaunch {
    BrokerLaunch {
        request_id: request_id(),
        target: OsBytes::from_os_string(launch.target.as_os_str()),
        args: args_after_subcommand(process_args)
            .iter()
            .map(|arg| OsBytes::from_os_string(arg))
            .collect(),
        cwd: std::env::current_dir()
            .ok()
            .map(|dir| OsBytes::from_os_string(dir.as_os_str())),
        env: env::snapshot(&env::ProcessEnv),
        console: false,
    }
}

fn request_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or(0);
    request_id_from(std::process::id(), nanos, &request_id as *const _ as usize)
}

fn request_id_from(pid: u32, nanos: u128, salt: usize) -> String {
    format!("warm-{pid:08x}-{nanos:x}-{salt:x}")
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsStr;

    fn launch(positionality: bool) -> Launch {
        Launch {
            target: std::path::PathBuf::from("app"),
            dev_url: None,
            devtools_port: None,
            app_args: Vec::new(),
            grant: None,
            no_prompt: false,
            trust_key: None,
            allow_unsigned: false,
            launch_window: false,
            positional: positionality,
        }
    }

    #[test]
    fn the_warm_attempt_applies_only_to_plain_positional_runs() {
        assert!(should_try(&launch(true), None));
        assert!(should_try(&launch(true), Some("1")));
        assert!(!should_try(&launch(false), None), "not the `run` form");
        // The emergency switch.
        assert!(!should_try(&launch(true), Some("0")));
        // Devtools and the development server always bypass the warm path.
        let mut devtools = launch(true);
        devtools.devtools_port = Some(7000);
        assert!(!should_try(&devtools, None));
        let mut dev = launch(true);
        dev.dev_url = Some(url::Url::parse("http://127.0.0.1:3000/").unwrap());
        assert!(!should_try(&dev, None));
    }

    #[test]
    fn request_ids_are_unique_enough_and_stable_in_shape() {
        let id = request_id_from(12, 5, 7);
        assert!(id.starts_with("warm-0000000c-5-7"));
        assert_ne!(request_id_from(1, 1, 1), request_id_from(1, 2, 1));
    }

    #[test]
    fn the_subcommand_is_stripped_only_from_the_front() {
        let args: Vec<OsString> = ["run", "app", "--flag"].map(OsString::from).to_vec();
        assert_eq!(
            args_after_subcommand(&args),
            [OsString::from("app"), OsString::from("--flag")]
        );
        let args: Vec<OsString> = ["app", "run"].map(OsString::from).to_vec();
        assert_eq!(args_after_subcommand(&args).len(), 2);
        assert_eq!(args_after_subcommand(&[]).len(), 0);
    }

    #[test]
    fn the_request_carries_the_exact_values() {
        let launch = launch(true);
        let args: Vec<OsString> = ["run", "app", "--x"].map(OsString::from).to_vec();
        let request = build_request(&args, &launch);
        assert_eq!(request.target, OsBytes::from_os_string(OsStr::new("app")));
        assert_eq!(request.args.len(), 2);
        assert_eq!(request.args[1], OsBytes::from_os_string(OsStr::new("--x")));
        assert!(request.console == false);
        assert!(!request.request_id.is_empty());
    }
}
