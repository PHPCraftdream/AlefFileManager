// SPDX-License-Identifier: MIT OR Apache-2.0
//! The launch pipeline shared by the directory and package sources.
use super::dispatch::{install_tls, relaunch_directory};
use super::failure::Failure;
use super::signals::end_on_signals;
use super::DEFAULT_ICON;
use alef_core::security::{
    authority::Authority,
    consent::{Decision, Identity},
    permissions::PathVars,
};
use alef_launch::{
    application::{
        pipeline,
        source::Prepared,
        trust::{self, TrustInputs},
    },
    args::Launch,
    consent::{
        ai_folder, identity_of, runtime_home, shadow_folder, store, watch, Asker, WATCH_EVERY,
    },
    plan::{
        application_base_dirs, launch_timeout_from_environment, load_manifest, make_package_plan,
        make_plan, package_path_vars, path_vars, Plan,
    },
    relaunch, runtimes,
};
use alef_modules::{
    desktop::args::{parse, Parsed},
    namespace_paths, register_all, AppInfo, Backends, Console, ModuleContext, Termination,
};
use alef_runtime::{mark, Bridge, BridgeOptions, Commands, WindowOptions};
use std::{ffi::OsString, path::PathBuf, time::Duration};
use url::Url;

/// Everything the one launch pipeline needs after the source is loaded and the plan derived.
pub(crate) struct Run {
    process_args: Vec<OsString>,
    dev_url: Option<Url>,
    devtools_port: Option<u16>,
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

/// A resolved launch, or an exit the relaunch decision already produced.
pub(crate) enum ResolvedRun {
    Run(Run),
    Exit(u8),
}

impl Run {
    /// Why the windowed warm spare refuses this launch before any
    /// irreversible action (docs, «Безопасная поздняя привязка»); the client
    /// then runs the unchanged cold path.
    pub(crate) fn warm_decline_reason(&self) -> Option<String> {
        if self.devtools_port.is_some() {
            return Some("--devtools cannot be warmed".to_owned());
        }
        if self.dev_url.is_some() {
            return Some("--dev-url cannot be warmed".to_owned());
        }
        if self.launch_window {
            return Some("--launch-mode window cannot be warmed".to_owned());
        }
        if self.plan.manifest.console {
            return Some("console applications are not warmed".to_owned());
        }
        if self.plan.windowless() {
            return Some("windowless applications are not warmed".to_owned());
        }
        // A package whose trust is not decided yet would open the trust
        // window — impossible in the spare; the client asks on its own.
        if let Some((signer, _, inputs)) = &self.trust {
            let noninteractive = inputs.trust_key.is_some() || inputs.allow_unsigned;
            if !noninteractive {
                let pinned = trust::load(&trust::trust_folder(), &self.plan.manifest.id)
                    .ok()
                    .flatten()
                    .is_some();
                if !pinned || signer.is_none() {
                    return Some("an interactive trust decision may be required".to_owned());
                }
            }
        }
        None
    }
}

/// Everything the end of a launch needs after the event loop exited. The bridge is
/// `Some` on the cold path (shutdown after the loop); the warm spare's bridge moves
/// into the event loop (`run_warm`) and is put here once the loop hands it back.
pub(crate) struct AppFinish {
    pub(crate) bridge: Option<Bridge>,
    pub(crate) console: Option<Console>,
    pub(crate) signal_task: tokio::task::JoinHandle<()>,
    pub(crate) handle: alef_runtime::RuntimeHandle,
    pub(crate) process_args: Vec<OsString>,
}

/// The prepared application: everything is built, only the event loop is missing.
pub(crate) struct PreparedApp {
    pub(crate) bridge: Bridge,
    pub(crate) options: WindowOptions,
    pub(crate) windowless: bool,
    pub(crate) launch_state: alef_modules::LaunchState,
    pub(crate) console: Option<Console>,
    pub(crate) signal_task: tokio::task::JoinHandle<()>,
    pub(crate) handle: alef_runtime::RuntimeHandle,
    pub(crate) process_args: Vec<OsString>,
}

/// The directory source: development, exactly as before packages existed.
pub(crate) async fn run_directory(
    process_args: Vec<OsString>,
    launch: Launch,
    dir: PathBuf,
) -> Result<ResolvedRun, Failure> {
    let Launch {
        dev_url,
        devtools_port,
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
            return relaunch_directory(requirement, &process_args)
                .await
                .map(ResolvedRun::Exit);
        }
    }
    let vars = path_vars(&dir, &manifest.id)?;
    let plan = make_plan(&dir, manifest, &vars)?;
    let identity = identity_of(&dir, &plan.manifest.id)?;
    let icon = std::fs::read(dir.join("icon.png")).unwrap_or_else(|_| DEFAULT_ICON.to_vec());
    Ok(ResolvedRun::Run(Run {
        process_args,
        dev_url,
        // `--devtools` rides in `process_args` verbatim for relaunches; the parsed port
        // drives this process's Servo preferences.
        devtools_port,
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
    }))
}

/// The package source: verified before anything is trusted or decided.
pub(crate) async fn run_package(
    process_args: Vec<OsString>,
    launch: Launch,
    prepared: Box<Prepared>,
) -> Result<ResolvedRun, Failure> {
    let Launch {
        dev_url,
        app_args,
        grant,
        no_prompt,
        trust_key,
        allow_unsigned,
        launch_window,
        // `--devtools` is refused for packages by `check` before this point.
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
    let installed = alef_launch::install::installed_app(
        &runtime_home(),
        &path,
        &manifest.id,
        &manifest.version,
    );
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
    Ok(ResolvedRun::Run(Run {
        process_args,
        dev_url,
        // Refused for packages by `check` before this point.
        devtools_port: None,
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
    }))
}

/// The pipeline both sources share: everything from the resolved launch up to, but not
/// including, the event loop. The cold path then runs the loop in [`cold_loop`]; the warm
/// spare hands `bridge` and `options` to [`alef_runtime::run_warm`] instead.
pub(crate) async fn prepare_app(run: Run) -> Result<Result<PreparedApp, u8>, Failure> {
    let Run {
        process_args,
        dev_url,
        devtools_port,
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
            return Ok(Err(0));
        }
    };
    let trust_asker = match &trust {
        Some(_) => Some(trust::asker_from_environment().map_err(Failure::Usage)?),
        None => None,
    };
    let asker = Asker::from_environment(grant, no_prompt).map_err(Failure::Usage)?;
    // The consent window, when it opens, is awaited inside `settle_launch`.
    mark("settle-begin");
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
    mark("settle-end");
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
                runtime_version: env!("CARGO_PKG_VERSION").to_owned(),
                installed_runtime_versions: runtimes::installed()
                    .map(|installed| {
                        installed
                            .into_iter()
                            .map(|runtime| runtime.version)
                            .collect()
                    })
                    .unwrap_or_default(),
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
    let ai_home = ai_folder(&identity);
    // The user may take a right back while the application runs.
    let _following = watch(granted.clone(), store(), identity, WATCH_EVERY);
    // Where the windows that ask for it (`restore: true`) are written down between runs.
    let window_state = vars.app_data.join("window-state.ktav");
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
        ai_home,
        console: (plan.manifest.console || !plan.manifest.modes.is_empty()).then(Console::process),
        termination: termination.clone(),
        launch: launch_state.clone(),
    };
    let console = context.console.clone();
    alef_modules::data::log::install_panic_hook(&context)?;

    install_tls()?;
    mark("tls-installed");
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
            devtools: devtools_port
                .map(|port| (port, plan.manifest.name.clone(), plan.manifest.id.clone())),
            headless_loaded: Some(Box::new(move || loaded.loaded())),
            headless_tick: Some(Box::new(move || tick.check_timeout())),
            ..BridgeOptions::default()
        },
    )
    .await?;
    mark("bridge-ready");
    eprintln!(
        "ALEF_READY app={} pid={}",
        plan.manifest.id,
        std::process::id()
    );
    mark("ready");
    if windowless {
        eprintln!("ALEF_MODE windowless console={}", plan.manifest.console);
    }
    let handle = bridge.handle();
    // A signal asks the application to end as `app.quit` does; a second one ends it.
    let signal_task = tokio::spawn(end_on_signals(termination, handle.clone()));
    let options = WindowOptions {
        windows: plan.manifest.windows.clone(),
        profile_dir: Some(servo_profile),
        devtools_port,
        app_id: plan.manifest.id.clone(),
        ..WindowOptions::new(plan.manifest.name.clone(), icon).remembering_windows_in(window_state)
    };
    Ok(Ok(PreparedApp {
        bridge,
        options,
        windowless,
        launch_state,
        console,
        signal_task,
        handle,
        process_args,
    }))
}

/// The cold event loop: the last step of the pipeline, exactly as before the warm split.
pub(crate) fn cold_loop(app: PreparedApp) -> (Result<(), Box<dyn std::error::Error>>, AppFinish) {
    let PreparedApp {
        mut bridge,
        options,
        windowless,
        launch_state,
        console,
        signal_task,
        handle,
        process_args,
    } = app;
    let result = if windowless {
        // No window: the entry document runs in a hidden WebView.
        launch_state.load_started();
        alef_runtime::run_headless(&mut bridge)
    } else {
        alef_runtime::run(&mut bridge, options)
    };
    (
        result,
        AppFinish {
            bridge: Some(bridge),
            console,
            signal_task,
            handle,
            process_args,
        },
    )
}

/// The end of the pipeline, after the event loop exited: the same order as before the
/// warm split — bridge shutdown, console drain, signals, relaunch, exit code.
pub(crate) async fn finish_app(
    finish: AppFinish,
    result: Result<(), Box<dyn std::error::Error>>,
) -> Result<u8, Failure> {
    let AppFinish {
        bridge,
        console,
        signal_task,
        handle,
        process_args,
    } = finish;
    if let Some(mut bridge) = bridge {
        bridge.shutdown().await?;
    }
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
