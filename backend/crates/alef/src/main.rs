// SPDX-License-Identifier: MIT OR Apache-2.0
//! The generic Alef runtime: `alef --app <directory> [--dev-url <url>]`.
use std::{ffi::OsString, path::PathBuf, process::ExitCode, time::Duration};

use alef_core::{
    error::AlefError,
    security::{
        authority::Authority,
        consent::{Decision, Identity},
        permissions::PathVars,
    },
};
use alef_launch::{
    application::{
        pipeline::{self, SettleFailure},
        source::{self, ApplicationSource, LoadFailure, Prepared},
        trust::{self, TrustFailure, TrustInputs},
        window as trust_window,
    },
    args::{parse_args, Command, Launch, USAGE},
    consent::{identity_of, runtime_home, shadow_folder, store, watch, Asker, WATCH_EVERY},
    permissions,
    plan::{
        application_base_dirs, launch_timeout_from_environment, load_manifest, make_package_plan,
        make_plan, package_path_vars, path_vars, Plan,
    },
    relaunch,
    runtimes::{self, StoreError},
};
use alef_modules::{
    desktop::args::{parse, Parsed},
    namespace_paths, register_all, AppInfo, Backends, Console, ModuleContext, Termination,
};
use alef_runtime::{Bridge, BridgeOptions, Commands, WindowOptions};
use url::Url;

#[path = "consent/window.rs"]
mod consent_window;

surfman::declare_surfman!();

/// The default window icon (the Alef logo) unless the application ships `icon.png`.
const DEFAULT_ICON: &[u8] = include_bytes!("../../../../frontend/public/logo-32x32.png");

/// Exit codes: 2 — the command line or the manifest is unusable, 3 — the user has not given the
/// rights the application asks for, 1 — the runtime failed.
enum Failure {
    Usage(String),
    Declined(String),
    Runtime(String),
}

impl Failure {
    /// The category of a source that cannot be loaded, at its own exit code.
    fn load(failure: LoadFailure) -> Self {
        match failure.exit {
            2 => Self::Usage(failure.to_string()),
            3 => Self::Declined(failure.to_string()),
            _ => Self::Runtime(failure.to_string()),
        }
    }

    /// Runtime store errors retain their category and operational/usage exit code.
    fn runtime_store(error: StoreError) -> Self {
        if error.exit() == 2 {
            Self::Usage(error.to_string())
        } else {
            Self::Runtime(error.to_string())
        }
    }

    /// Trust that was not established: unusable input, refusal, or a store failure.
    fn trust(failure: TrustFailure) -> Self {
        match failure {
            TrustFailure::BadTrustKey(message) => {
                Self::Usage(format!("INVALID_ARGUMENT: {message}"))
            }
            TrustFailure::Refused(message) => Self::Declined(message),
            TrustFailure::Store(error) => Self::Runtime(error.to_string()),
        }
    }
}

impl From<SettleFailure> for Failure {
    /// The shared pipeline: documents, trust, consent, enforcement.
    fn from(failure: SettleFailure) -> Self {
        match failure {
            SettleFailure::Documents(failure) => Self::load(failure),
            SettleFailure::Trust(failure) => Self::trust(failure),
            SettleFailure::Unanswered(unanswered) => Self::Declined(unanswered.to_string()),
            SettleFailure::Store(error) => Self::Runtime(error.to_string()),
            SettleFailure::Enforcement(message) => Self::Runtime(message),
        }
    }
}

impl From<AlefError> for Failure {
    fn from(error: AlefError) -> Self {
        Self::Usage(format!("{}: {}", error.code.as_str(), error.message))
    }
}

impl From<std::io::Error> for Failure {
    fn from(error: std::io::Error) -> Self {
        Self::Runtime(error.to_string())
    }
}

fn install_failure(error: alef_launch::install::InstallError) -> Failure {
    match error.exit() {
        2 => Failure::Usage(error.to_string()),
        3 => Failure::Declined(error.to_string()),
        _ => Failure::Runtime(error.to_string()),
    }
}

fn main() -> ExitCode {
    // Before any thread exists: the relaunch marker never reaches the applications' children.
    relaunch::runtime::take_marker();
    // The predecessor pid is consumed once and never reaches the applications' children.
    relaunch::take_replaces_pid();
    let runtime = tokio::runtime::Builder::new_multi_thread()
        .worker_threads(2)
        .enable_all()
        .build()
        .expect("the async runtime starts");
    runtime.block_on(launch_main())
}

async fn launch_main() -> ExitCode {
    warn_end_to_end_build();
    let arguments: Vec<OsString> = std::env::args_os().skip(1).collect();
    match launch(arguments).await {
        Ok(code) => ExitCode::from(code),
        Err(Failure::Usage(message)) => {
            eprintln!("alef: {message}");
            ExitCode::from(2)
        }
        Err(Failure::Declined(message)) => {
            eprintln!("alef: {message}");
            ExitCode::from(3)
        }
        Err(Failure::Runtime(message)) => {
            eprintln!("alef: {message}");
            ExitCode::from(1)
        }
    }
}

#[cfg(feature = "e2e")]
const HOOKS_NOTICE: &str = "this build carries the end-to-end hooks";

#[cfg(feature = "e2e")]
fn hooks_notice(mode: Option<&str>) -> Option<&'static str> {
    (mode == Some("1")).then_some(HOOKS_NOTICE)
}

#[cfg(feature = "e2e")]
fn warn_end_to_end_build() {
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
fn warn_end_to_end_build() {}

fn install_tls() -> Result<(), Failure> {
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .map_err(|_| Failure::Runtime("Failed to configure the Servo TLS provider".to_owned()))
}

/// Waits for the signals that ask a process to end (Ctrl+C, SIGTERM, Ctrl+Break, the closing of the
/// console) and asks the application to quit the way `app.quit` does; the code is 128 and the number of the
/// signal where there is one. When the documents veto, or when another signal comes, the process ends.
async fn end_on_signals(termination: Termination, handle: alef_runtime::RuntimeHandle) {
    let Ok(mut signals) = signals() else {
        eprintln!("alef: the signals cannot be watched");
        return;
    };
    let mut asked = false;
    while let Some(code) = signals.recv().await {
        if asked {
            alef_core::registry::host::Host::quit(&handle, code);
            return;
        }
        if termination.request(code).await {
            return;
        }
        asked = true;
    }
}

/// The signals as exit codes, one by one.
fn signals() -> std::io::Result<tokio::sync::mpsc::Receiver<i32>> {
    use tokio::sync::mpsc;
    let (sender, receiver) = mpsc::channel(4);
    #[cfg(unix)]
    {
        use tokio::signal::unix::{signal, SignalKind};
        for (kind, code) in [
            (SignalKind::interrupt(), 130),
            (SignalKind::terminate(), 143),
            (SignalKind::hangup(), 129),
        ] {
            let mut stream = signal(kind)?;
            let sender = sender.clone();
            tokio::spawn(async move {
                while stream.recv().await.is_some() {
                    if sender.send(code).await.is_err() {
                        break;
                    }
                }
            });
        }
    }
    #[cfg(windows)]
    {
        use tokio::signal::windows::{ctrl_break, ctrl_c, ctrl_close, ctrl_shutdown};
        macro_rules! watch {
            ($stream:expr, $code:expr) => {{
                let mut stream = $stream?;
                let sender = sender.clone();
                tokio::spawn(async move {
                    while stream.recv().await.is_some() {
                        if sender.send($code).await.is_err() {
                            break;
                        }
                    }
                });
            }};
        }
        watch!(ctrl_c(), 130);
        watch!(ctrl_break(), 143);
        watch!(ctrl_close(), 143);
        watch!(ctrl_shutdown(), 143);
    }
    Ok(receiver)
}

/// Runs the application; the result is the exit code the application asked for.
async fn launch(arguments: Vec<OsString>) -> Result<u8, Failure> {
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
        Command::Run(launch) => launch,
    };
    let loaded = match source::load(&launch.target) {
        Ok(loaded) => loaded,
        Err(failure) => {
            if let Some(requirement) = failure.runtime_requirement.clone() {
                // The relaunch decision comes before the diagnostic; an impossible relaunch
                // falls back to the original failure with its category and exit code.
                match relaunch::runtime::attempt(&requirement, &process_args).await {
                    Ok(code) => return Ok(code),
                    Err(relaunch::runtime::AttemptFailure::Incompatible(_)) => {
                        return Err(Failure::load(failure));
                    }
                    Err(relaunch::runtime::AttemptFailure::Start(reason)) => {
                        return Err(Failure::Runtime(format!(
                            "RUNTIME_RELAUNCH_FAILED: {reason}"
                        )));
                    }
                }
            }
            return Err(Failure::load(failure));
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

/// The directory source has no load failure to fall back to: the diagnostic comes from the
/// same command module detail the package path uses.
async fn relaunch_directory(requirement: &str, process_args: &[OsString]) -> Result<u8, Failure> {
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

/// Everything the one launch pipeline needs after the source is loaded and the plan derived.
struct Run {
    process_args: Vec<OsString>,
    dev_url: Option<Url>,
    app_args: Vec<OsString>,
    grant: Option<Decision>,
    no_prompt: bool,
    launch_window: bool,
    plan: Plan,
    vars: PathVars,
    identity: Identity,
    authority: Authority,
    icon: Vec<u8>,
    /// Package trust: the signer (key, fingerprint), the archive digest, and the inputs.
    trust: Option<(Option<(String, String)>, String, TrustInputs)>,
    /// Set only when the package came from the user store of `alef install`.
    installed: Option<alef_modules::net::updater::InstalledApp>,
}

/// The directory source: development, exactly as before packages existed.
async fn run_directory(
    process_args: Vec<OsString>,
    launch: Launch,
    dir: PathBuf,
) -> Result<u8, Failure> {
    let Launch {
        dev_url,
        app_args,
        grant,
        no_prompt,
        launch_window,
        ..
    } = launch;
    let manifest = load_manifest(&dir)?;
    if let Some(requirement) = manifest.runtime.as_deref() {
        if !alef_core::security::manifest::runtime_requirement_met(
            requirement,
            env!("CARGO_PKG_VERSION"),
        ) {
            return relaunch_directory(requirement, &process_args).await;
        }
    }
    let vars = path_vars(&dir, &manifest.id)?;
    let plan = make_plan(&dir, manifest, &vars)?;
    let identity = identity_of(&dir, &plan.manifest.id)?;
    let icon = std::fs::read(dir.join("icon.png")).unwrap_or_else(|_| DEFAULT_ICON.to_vec());
    run_app(Run {
        process_args,
        dev_url,
        app_args,
        grant,
        no_prompt,
        launch_window,
        plan,
        vars,
        identity,
        authority: Authority::default(),
        icon,
        trust: None,
        installed: None,
    })
    .await
}

/// The package source: verified before anything is trusted or decided.
async fn run_package(
    process_args: Vec<OsString>,
    launch: Launch,
    prepared: Box<Prepared>,
) -> Result<u8, Failure> {
    let Launch {
        dev_url,
        app_args,
        grant,
        no_prompt,
        trust_key,
        allow_unsigned,
        launch_window,
        ..
    } = launch;
    let identity = prepared.identity();
    let signer = prepared
        .fingerprint()
        .zip(prepared.public_key_hex().map(|key| key.to_owned()))
        .map(|(fingerprint, key)| (key, fingerprint.to_owned()));
    let authority = prepared.authority.clone();
    let archive_digest = prepared.archive_digest.clone();
    let icon = prepared
        .icon_png
        .clone()
        .unwrap_or_else(|| DEFAULT_ICON.to_vec());
    let Prepared {
        manifest,
        package,
        path,
        selected_root,
        ..
    } = *prepared;
    let installed = store_installed(&path, &manifest.id, &manifest.version);
    let mut vars = package_path_vars(&manifest.id)?;
    // Package data lives under the authority token, not the manifest id; the modules check this.
    let (data_base, config_base, cache_base) = application_base_dirs(&vars.home);
    namespace_paths(
        &mut vars,
        &data_base,
        &config_base,
        &cache_base,
        &manifest.id,
        &authority,
    );
    let plan = make_package_plan(package, &selected_root, manifest, &vars)?;
    run_app(Run {
        process_args,
        dev_url,
        app_args,
        grant,
        no_prompt,
        launch_window,
        plan,
        vars,
        identity,
        authority,
        icon,
        trust: Some((
            signer,
            archive_digest,
            TrustInputs {
                trust_key,
                allow_unsigned,
            },
        )),
        installed,
    })
    .await
}

/// Only a package the store of `alef install` runs may apply an update: the running file must be
/// the package a valid `app.json` record of exactly this id and version references.
fn store_installed(
    package: &std::path::Path,
    id: &str,
    version: &str,
) -> Option<alef_modules::net::updater::InstalledApp> {
    use sha2::Digest as _;
    let hash = format!("{:x}", sha2::Sha256::digest(id.as_bytes()));
    let app_dir = runtime_home().join("apps").join(&hash);
    // read_record fails unless app.json exists, is plain, bounded, and passes its semantic
    // validation (id hash, `package` = `<version>.alef`, absolute executable).
    let record = alef_launch::install::read_record(&app_dir, &hash).ok()?;
    if record.id != id || record.version != version {
        return None;
    }
    // Canonical comparison: a relative or linked path on the command line still matches, and
    // nothing else in the store is mistaken for the running package.
    let running = std::fs::canonicalize(package).ok()?;
    let stored = std::fs::canonicalize(app_dir.join(&record.package)).ok()?;
    (running == stored).then(|| alef_modules::net::updater::InstalledApp { package: stored })
}

/// The pipeline both sources share; the result is the exit code the application asked for.
async fn run_app(run: Run) -> Result<u8, Failure> {
    let Run {
        process_args,
        dev_url,
        app_args,
        grant,
        no_prompt,
        launch_window,
        plan,
        vars,
        identity,
        authority,
        icon,
        trust,
        installed,
    } = run;
    // A restart into an update waits for the process it replaces to release its single-instance
    // claim, so the new instance is never told "you are the second one" by the dying old one.
    let replaces = relaunch::replaces_pid();
    if replaces != 0 && !relaunch::wait_for_process_exit(replaces, Duration::from_secs(15)) {
        return Err(Failure::Runtime(
            "UPDATE_RELAUNCH_FAILED: the replaced process did not exit in time".to_owned(),
        ));
    }
    let mut plan = plan;
    if launch_window {
        plan.select_window_child()?;
    }
    let limit = launch_timeout_from_environment().map_err(Failure::Usage)?;
    let launch_state =
        alef_modules::LaunchState::new(plan.allowed_modes(), plan.initial_mode(), limit)?;
    let (app_args, startup_urls) =
        alef_modules::desktop::args::extract_deep_links(&plan.manifest.deep_links, &app_args)?;
    let args = match parse(
        plan.manifest.arguments.as_ref(),
        &plan.manifest.name,
        &plan.manifest.version,
        &app_args,
    )? {
        Parsed::Run(args) => args,
        Parsed::Help(text) | Parsed::Version(text) => {
            println!("{text}");
            return Ok(0);
        }
    };
    let trust_asker = match &trust {
        Some(_) => Some(trust::asker_from_environment().map_err(Failure::Usage)?),
        None => None,
    };
    let asker = Asker::from_environment(grant, no_prompt).map_err(Failure::Usage)?;
    let settled = pipeline::settle_launch(
        plan,
        &identity,
        trust.as_ref(),
        trust_asker.as_ref(),
        &asker,
        &store(),
        dev_url.as_ref(),
    )
    .await?;
    let pipeline::SettledLaunch {
        plan,
        assets,
        consent: _,
        granted,
    } = settled;
    let termination = Termination::default();
    // The updater context is only built when the pin matches the running signer; the applier
    // reuses the launcher store and is pinned to that same signer.
    let updater_context = match (&authority, trust.as_ref(), plan.manifest.updater.clone()) {
        (
            Authority::SignedPackage { fingerprint },
            Some((Some((_, expected_signer)), _, inputs)),
            Some(config),
        ) if fingerprint == expected_signer
            && inputs
                .trust_key
                .as_deref()
                .map_or(true, |given| given == expected_signer) =>
        {
            let pin = trust::load(&trust::trust_folder(), &plan.manifest.id)
                .map_err(|error| Failure::Runtime(format!("updater trust pin: {error}")))?
                .ok_or_else(|| Failure::Runtime("updater trust pin is missing".to_owned()))?;
            if pin.fingerprint != *fingerprint || pin.fingerprint != *expected_signer {
                return Err(Failure::Runtime(
                    "updater trust pin does not match package signer".to_owned(),
                ));
            }
            let mut key = [0u8; 32];
            for (index, byte) in key.iter_mut().enumerate() {
                *byte = u8::from_str_radix(&pin.public_key[index * 2..index * 2 + 2], 16)
                    .map_err(|_| Failure::Runtime("invalid updater trust pin key".to_owned()))?;
            }
            let executable = std::env::current_exe()
                .map_err(|e| Failure::Runtime(format!("updater executable: {e}")))?;
            Some(alef_modules::net::updater::UpdaterContext {
                config,
                app_id: plan.manifest.id.clone(),
                version: plan.manifest.version.clone(),
                trusted_key: key,
                data: vars.app_data.clone(),
                installed,
                applier: std::sync::Arc::new(alef_launch::install::StoreApplier::native(
                    runtime_home(),
                    executable,
                    pin.fingerprint.clone(),
                    termination.clone(),
                )),
            })
        }
        _ => None,
    };
    let shadow = shadow_folder(&identity);
    // The user may take a right back while the application runs.
    let _following = watch(granted.clone(), store(), identity, WATCH_EVERY);
    // Where the windows that ask for it (`restore: true`) are written down between runs.
    let window_state = vars.app_data.join("window-state.json");
    // Servo profile root, before `vars` moves into the module context.
    let servo_profile = vars.app_data.join("servo");
    // Directory apps derive the origin label from the manifest id; packages from the
    // launcher-verified authority token. The label feeds both the entry URL and the
    // CSP-scoped origin, so a bad id must exit like any other manifest failure.
    let origin_host = alef_core::security::origin::origin_host(&plan.manifest.id, &authority)
        .map_err(Failure::from)?;
    // The interception exempts exactly the sources the manifest/CSP allows; wildcards
    // (`*.example.com`) are kept as sources and resolved per request by `origin_matches`.
    let external = &plan.manifest.external;
    let allowed_external_origins: Vec<String> = external
        .connect
        .iter()
        .chain(&external.load.scripts)
        .chain(&external.load.styles)
        .chain(&external.load.images)
        .chain(&external.load.fonts)
        .chain(&external.load.media)
        .chain(&external.load.frames)
        .filter_map(|entry| alef_core::security::csp::normalize_source(entry))
        .collect();
    let context = ModuleContext {
        authority,
        app: AppInfo {
            id: plan.manifest.id.clone(),
            name: plan.manifest.name.clone(),
            version: plan.manifest.version.clone(),
            runtime_version: env!("CARGO_PKG_VERSION").to_owned(),
        },
        paths: vars,
        args,
        deep_link_schemes: plan.manifest.deep_links.clone(),
        startup_urls,
        process_args: process_args.clone(),
        backends: Backends::from_environment(&plan.manifest.name),
        shadow,
        console: (plan.manifest.console || !plan.manifest.modes.is_empty()).then(Console::process),
        termination: termination.clone(),
        launch: launch_state.clone(),
    };
    let console = context.console.clone();

    install_tls()?;
    let allowed_origins = dev_url
        .iter()
        .map(|url| url.origin().ascii_serialization())
        .collect();
    let (entry, windowless) = (plan.entry(), plan.windowless());
    let loaded = launch_state.clone();
    let tick = launch_state.clone();
    let mut bridge = Bridge::with_options(
        Commands::new(),
        assets,
        dev_url,
        BridgeOptions {
            modules: Some(Box::new(move |registry, host| {
                register_all(registry, host, &context)?;
                alef_modules::net::updater::register_updater(registry, &updater_context)
            })),
            allowed_origins,
            csp: Some(plan.csp),
            permissions: Some(granted),
            entry: Some(entry),
            initial_document_urls: plan
                .manifest
                .windows
                .iter()
                .map(|window| window.url.clone())
                .collect(),
            origin_host: Some(origin_host.clone()),
            allowed_external_origins,
            profile_dir: Some(servo_profile.clone()),
            headless_loaded: Some(Box::new(move || loaded.loaded())),
            headless_tick: Some(Box::new(move || tick.check_timeout())),
            ..BridgeOptions::default()
        },
    )
    .await?;
    eprintln!(
        "ALEF_READY app={} pid={}",
        plan.manifest.id,
        std::process::id()
    );
    if windowless {
        eprintln!("ALEF_MODE windowless console={}", plan.manifest.console);
    }
    let handle = bridge.handle();
    // A signal asks the application to end as `app.quit` does; a second one ends it.
    let signal_task = tokio::spawn(end_on_signals(termination, handle.clone()));
    let result = if windowless {
        // No window: the entry document runs in a hidden WebView.
        launch_state.load_started();
        alef_runtime::run_headless(&mut bridge)
    } else {
        alef_runtime::run(
            &mut bridge,
            WindowOptions {
                windows: plan.manifest.windows.clone(),
                profile_dir: Some(servo_profile),
                ..WindowOptions::new(plan.manifest.name.clone(), icon)
                    .remembering_windows_in(window_state)
            },
        )
    };
    bridge.shutdown().await?;
    if let Some(console) = &console {
        // What the page wrote to stdout and stderr leaves before the process does.
        console.drain().await;
    }
    signal_task.abort();
    let _ = signal_task.await;
    result.map_err(|error| Failure::Runtime(error.to_string()))?;
    if handle.relaunch_window_requested() {
        return relaunch::window(&process_args).await.map_err(Failure::from);
    }
    Ok(u8::try_from(handle.exit_code()).unwrap_or(1))
}

#[cfg(test)]
mod tests {
    use super::*;
    use alef_launch::consent::{Unanswered, Why};

    #[test]
    fn a_load_failure_maps_to_its_own_exit_code_and_prints_its_category() {
        for (category, exit) in [
            (Some("NOT_A_PACKAGE"), 2),
            (Some("PACKAGE_FORMAT"), 2),
            (Some("PACKAGE_INTEGRITY"), 2),
            (Some("PACKAGE_SIGNATURE"), 2),
            (Some("PACKAGE_MANIFEST"), 2),
            (Some("MANIFEST_INVALID"), 2),
            (Some("RUNTIME_INCOMPATIBLE"), 2),
            (Some("PACKAGE_RUN_LIMIT"), 2),
            (Some("SIGNATURE_REQUIRED"), 3),
            (None, 1),
        ] {
            let failure = Failure::load(LoadFailure {
                category,
                detail: "detail".to_owned(),
                exit,
                runtime_requirement: None,
            });
            match &failure {
                Failure::Usage(message) if exit == 2 => {
                    if let Some(category) = category {
                        assert!(message.contains(category), "{message}");
                    }
                }
                Failure::Declined(message) if exit == 3 => {
                    assert!(message.contains("SIGNATURE_REQUIRED"), "{message}");
                    assert!(message.contains("detail"), "{message}");
                }
                Failure::Runtime(message) if exit == 1 => {
                    assert!(category.is_none());
                    assert!(message.contains("detail"), "{message}");
                }
                _ => panic!("exit {exit}: wrong variant"),
            }
        }
    }

    #[test]
    fn runtime_store_failures_preserve_categories_and_usage_or_operational_exit_codes() {
        for (error, category, exit) in [
            (
                StoreError::InvalidArgument("invalid".to_owned()),
                "INVALID_ARGUMENT",
                2,
            ),
            (
                StoreError::AlreadyInstalled("present".to_owned()),
                "ALREADY_INSTALLED",
                2,
            ),
            (
                StoreError::Store(std::io::Error::other("unreadable")),
                "RUNTIME_STORE",
                1,
            ),
        ] {
            assert_eq!(error.exit(), exit);
            match Failure::runtime_store(error) {
                Failure::Usage(message) if exit == 2 => {
                    assert!(message.starts_with(&format!("{category}: ")))
                }
                Failure::Runtime(message) if exit == 1 => {
                    assert_eq!(message, "RUNTIME_STORE: unreadable")
                }
                _ => panic!("wrong failure variant for exit {exit}"),
            }
        }
    }

    #[test]
    fn a_failed_relaunch_start_is_an_operational_runtime_relaunch_failed_failure() {
        let Failure::Runtime(message) =
            Failure::Runtime("RUNTIME_RELAUNCH_FAILED: denied".to_owned())
        else {
            panic!("a start failure is operational")
        };
        assert!(
            message.starts_with("RUNTIME_RELAUNCH_FAILED: "),
            "{message}"
        );
    }

    #[test]
    fn a_trust_failure_maps_to_unusable_refused_or_operational() {
        let Failure::Usage(message) = Failure::trust(TrustFailure::BadTrustKey("x".to_owned()))
        else {
            panic!("a bad trust key is unusable input");
        };
        assert!(message.contains("INVALID_ARGUMENT: x"), "{message}");
        let Failure::Declined(message) =
            Failure::trust(TrustFailure::Refused("SIGNER_CHANGED: r".to_owned()))
        else {
            panic!("a refusal is declined");
        };
        assert!(message.contains("SIGNER_CHANGED"), "{message}");
        let Failure::Runtime(message) = Failure::trust(TrustFailure::Store(std::io::Error::new(
            std::io::ErrorKind::Other,
            "io",
        ))) else {
            panic!("a store failure is operational");
        };
        assert!(message.contains("io"), "{message}");
    }

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

    #[test]
    fn a_settlement_failure_maps_to_its_own_exit_code_family() {
        let Failure::Usage(message) = Failure::from(SettleFailure::Documents(LoadFailure {
            category: Some("MANIFEST_INVALID"),
            detail: "assets: /index.html: missing".to_owned(),
            exit: 2,
            runtime_requirement: None,
        })) else {
            panic!("a document failure is unusable input");
        };
        assert!(message.contains("MANIFEST_INVALID"), "{message}");
        let Failure::Usage(_) = Failure::from(SettleFailure::Trust(TrustFailure::BadTrustKey(
            "k".to_owned(),
        ))) else {
            panic!("a bad trust key is unusable input");
        };
        let Failure::Declined(_) = Failure::from(SettleFailure::Unanswered(Unanswered {
            rights: Vec::new(),
            why: Why::Nobody,
        })) else {
            panic!("unanswered rights are declined");
        };
        let Failure::Runtime(_) = Failure::from(SettleFailure::Store(std::io::Error::new(
            std::io::ErrorKind::Other,
            "io",
        ))) else {
            panic!("a store failure is operational");
        };
        let Failure::Runtime(_) = Failure::from(SettleFailure::Enforcement("e".to_owned())) else {
            panic!("an enforcement failure is operational");
        };
    }
}
