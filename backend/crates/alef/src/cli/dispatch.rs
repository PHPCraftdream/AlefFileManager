// SPDX-License-Identifier: MIT OR Apache-2.0
//! Command dispatch: the subcommands that answer and exit, then the run path.
use super::app_run::{run_directory, run_package};
use super::failure::{install_failure, Failure};
use super::DEFAULT_ICON;
use alef_launch::{
    application::{
        source::{self, ApplicationSource, LoadFailure},
        window as trust_window,
    },
    args::{parse_args, Command, USAGE},
    consent::store,
    permissions, relaunch, runtimes,
};
use alef_runtime::mark;
use std::ffi::OsString;

#[path = "../../apps/window.rs"]
mod apps_window;
#[path = "../consent/window.rs"]
mod consent_window;

#[cfg(feature = "e2e")]
const HOOKS_NOTICE: &str = "this build carries the end-to-end hooks";
#[cfg(feature = "e2e")]
fn hooks_notice(mode: Option<&str>) -> Option<&'static str> {
    (mode == Some("1")).then_some(HOOKS_NOTICE)
}
#[cfg(feature = "e2e")]
pub(crate) fn warn_end_to_end_build() {
    if let Some(notice) = hooks_notice(std::env::var("ALEF_E2E").ok().as_deref()) {
        eprintln!("{notice}");
    }
}
#[cfg(all(test, feature = "e2e"))]
#[test]
fn the_notice_of_the_hooks_is_given_only_in_an_end_to_end_run() {
    assert_eq!(
        hooks_notice(Some("1")),
        Some("this build carries the end-to-end hooks")
    );
    assert_eq!(hooks_notice(Some("0")), None);
    assert_eq!(hooks_notice(None), None);
}

#[cfg(not(feature = "e2e"))]
pub(crate) fn warn_end_to_end_build() {}

pub(crate) fn install_tls() -> Result<(), Failure> {
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .map_err(|_| Failure::Runtime("Failed to configure the Servo TLS provider".to_owned()))
}

/// Runs the application; the result is the exit code the application asked for.
pub(crate) async fn launch(arguments: Vec<OsString>) -> Result<u8, Failure> {
    let process_args = arguments.clone();
    let launch = match parse_args(arguments).map_err(Failure::Usage)? {
        Command::Help => {
            println!("{USAGE}");
            return Ok(0);
        }
        Command::Permissions(command) => {
            print!(
                "{}",
                permissions::run(&command, &store()).map_err(Failure::Usage)?
            );
            return Ok(0);
        }
        Command::Ai(command) => {
            print!("{}", permissions::run_ai(&command).map_err(Failure::Usage)?);
            return Ok(0);
        }
        Command::Runtime(command) => {
            print!(
                "{}",
                runtimes::command::run(&command).map_err(Failure::runtime_store)?
            );
            return Ok(0);
        }
        Command::Consent { request, answer } => {
            install_tls()?;
            consent_window::run(&request, &answer, DEFAULT_ICON.to_vec())
                .await
                .map_err(Failure::Runtime)?;
            return Ok(0);
        }
        Command::Trust { request, answer } => {
            install_tls()?;
            trust_window::run(&request, &answer, DEFAULT_ICON.to_vec())
                .await
                .map_err(Failure::Runtime)?;
            return Ok(0);
        }
        Command::Install {
            package,
            trust_key,
            allow_unsigned,
        } => {
            let message = alef_launch::install::install(&package, trust_key, allow_unsigned)
                .await
                .map_err(|e| install_failure(e))?;
            println!("{message}");
            return Ok(0);
        }
        Command::Uninstall { id, purge_data } => {
            alef_launch::install::uninstall(&id, purge_data).map_err(install_failure)?;
            return Ok(0);
        }
        Command::Apps { json } => {
            println!(
                "{}",
                alef_launch::install::apps(json).map_err(install_failure)?
            );
            return Ok(0);
        }
        Command::AppsUi => {
            install_tls()?;
            apps_window::run(DEFAULT_ICON.to_vec())
                .await
                .map_err(Failure::Runtime)?;
            return Ok(0);
        }
        Command::Run(launch) => launch,
    };
    mark("args-parsed");
    let mut launch = launch;
    if let alef_launch::install::RunTarget::Id(id) =
        alef_launch::install::run_target(&launch.target, launch.positional)
    {
        return run_installed(&id, launch, process_args).await;
    }
    #[cfg(feature = "e2e")]
    if alef_launch::install::print_run_target_hook(&launch.target) {
        return Ok(0);
    }
    let loaded = match source::load(&launch.target) {
        Ok(loaded) => loaded,
        Err(failure) => return load_failure_exit(failure, &process_args).await,
    };
    match loaded {
        ApplicationSource::Directory { dir } => {
            launch.check(false).map_err(Failure::Usage)?;
            run_directory(process_args, launch, dir).await
        }
        ApplicationSource::Package(prepared) => {
            launch.check(true).map_err(Failure::Usage)?;
            run_package(process_args, launch, prepared).await
        }
    }
}

/// Directory source runtime compatibility check. The relaunched child receives
/// `process_args` verbatim: `--devtools[=PORT]` is already carried there when the user gave
/// it (retained with `--dev-url`, absent otherwise), and appending a synthesized flag could
/// land after a URL (parser refuses) or after `--` (silently becomes an app argument).
pub(crate) async fn relaunch_directory(
    requirement: &str,
    process_args: &[OsString],
) -> Result<u8, Failure> {
    match relaunch::runtime::attempt(requirement, process_args).await {
        Ok(code) => Ok(code),
        Err(relaunch::runtime::AttemptFailure::Incompatible(detail)) => {
            Err(Failure::Usage(format!("RUNTIME_INCOMPATIBLE: {detail}")))
        }
        Err(relaunch::runtime::AttemptFailure::Start(reason)) => Err(Failure::Runtime(format!(
            "RUNTIME_RELAUNCH_FAILED: {reason}"
        ))),
    }
}

/// A failed load: the runtime relaunch decision comes before the diagnostic (the same
/// rule the package path follows).
async fn load_failure_exit(failure: LoadFailure, process_args: &[OsString]) -> Result<u8, Failure> {
    if let Some(requirement) = failure.runtime_requirement.clone() {
        // The relaunch decision comes before the diagnostic; an impossible relaunch
        // falls back to the original failure with its category and exit code.
        match relaunch::runtime::attempt(&requirement, process_args).await {
            Ok(code) => return Ok(code),
            Err(relaunch::runtime::AttemptFailure::Incompatible(_)) => {}
            Err(relaunch::runtime::AttemptFailure::Start(reason)) => {
                return Err(Failure::Runtime(format!(
                    "RUNTIME_RELAUNCH_FAILED: {reason}"
                )));
            }
        }
    }
    Err(Failure::load(failure))
}

/// The `alef run <ID>` path: the recorded package through the same pipeline as a path.
async fn run_installed(
    id: &str,
    launch: alef_launch::args::Launch,
    process_args: Vec<OsString>,
) -> Result<u8, Failure> {
    let mut resolved = None;
    let loaded = alef_launch::install::run_id(id, &mut |path| {
        resolved = Some(path.to_owned());
        source::load(path)
    });
    // The end-to-end hook reports the package the record resolved to, without starting it.
    if let Some(path) = resolved {
        #[cfg(feature = "e2e")]
        if alef_launch::install::print_run_target_hook(&path) {
            return Ok(0);
        }
        #[cfg(not(feature = "e2e"))]
        drop(path);
    }
    let loaded = match loaded {
        Ok(loaded) => loaded,
        Err(alef_launch::install::RunLaunchFailure::Resolve(error)) => {
            return Err(install_failure(error));
        }
        Err(alef_launch::install::RunLaunchFailure::Load(failure)) => {
            return load_failure_exit(failure, &process_args).await;
        }
    };
    match loaded {
        ApplicationSource::Directory { dir } => {
            launch.check(false).map_err(Failure::Usage)?;
            run_directory(process_args, launch, dir).await
        }
        ApplicationSource::Package(prepared) => {
            launch.check(true).map_err(Failure::Usage)?;
            run_package(process_args, launch, prepared).await
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_trust_and_consent_subcommands_reach_their_windows_without_touching_a_source() {
        let trust = parse_args(["trust", "in", "out"].map(OsString::from)).unwrap();
        assert!(matches!(
            trust,
            Command::Trust {
                request: _,
                answer: _
            }
        ));
        let consent = parse_args(["consent", "in", "out"].map(OsString::from)).unwrap();
        assert!(matches!(
            consent,
            Command::Consent {
                request: _,
                answer: _
            }
        ));
    }
}
