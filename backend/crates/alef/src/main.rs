// SPDX-License-Identifier: MIT OR Apache-2.0
//! The generic Alef runtime: `alef --app <directory> [--dev-url <url>]`.
use std::{ffi::OsString, process::ExitCode, sync::Arc};

use alef_core::{error::AlefError, security::consent::ConsentStore};
use alef_launch::{
    args::{parse_args, Command, Launch, USAGE},
    consent::{
        ask::AppSummary, enforce, identity_of, runtime_home, settle, store, watch, Asker,
        WATCH_EVERY,
    },
    permissions,
    plan::{load_manifest, make_plan, path_vars},
};
use alef_modules::{
    desktop::args::{parse, Parsed},
    register_all, AppInfo, Backends, ModuleContext,
};
use alef_runtime::{Bridge, BridgeOptions, Commands, WindowOptions};

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

#[tokio::main(flavor = "multi_thread", worker_threads = 2)]
async fn main() -> ExitCode {
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

fn install_tls() -> Result<(), Failure> {
    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .map_err(|_| Failure::Runtime("Failed to configure the Servo TLS provider".to_owned()))
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
        Command::Consent { request, answer } => {
            install_tls()?;
            consent_window::run(&request, &answer, DEFAULT_ICON.to_vec())
                .await
                .map_err(Failure::Runtime)?;
            return Ok(0);
        }
        Command::Run(launch) => launch,
    };
    let Launch {
        app_dir,
        dev_url,
        app_args,
        grant,
        no_prompt,
    } = launch;
    let manifest = load_manifest(&app_dir)?;
    let vars = path_vars(&app_dir, &manifest.id)?;
    let plan = make_plan(&app_dir, manifest, &vars)?;
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
    // The user decides what the application gets of what its manifest asks for.
    let identity = identity_of(&app_dir, &plan.manifest.id)?;
    let decisions = store();
    let asker = Asker::from_environment(grant, no_prompt).map_err(Failure::Usage)?;
    let summary = AppSummary {
        id: plan.manifest.id.clone(),
        name: plan.manifest.name.clone(),
        version: plan.manifest.version.clone(),
    };
    let settled = settle(
        &summary,
        &plan.permissions.rights(),
        decisions.load(&identity)?,
        &asker,
    )
    .map_err(|unanswered| Failure::Declined(unanswered.to_string()))?;
    if settled.changed {
        decisions.save(&identity, &settled.consent)?;
    }
    let settled_consent = settled.consent;
    let granted = Arc::new(
        (*plan.permissions)
            .clone()
            .with_consent(settled_consent.clone())
            .with_protected(&runtime_home()),
    );
    enforce(&granted, &settled_consent).map_err(Failure::Runtime)?;
    // The user may take a right back while the application runs.
    let _following = watch(granted.clone(), store(), identity, WATCH_EVERY);
    // Where the windows that ask for it (`restore: true`) are written down between runs.
    let window_state = vars.app_data.join("window-state.json");
    let context = ModuleContext {
        app: AppInfo {
            id: plan.manifest.id.clone(),
            name: plan.manifest.name.clone(),
            version: plan.manifest.version.clone(),
            runtime_version: env!("CARGO_PKG_VERSION").to_owned(),
        },
        paths: vars,
        args,
        process_args,
        backends: Backends::from_environment(&plan.manifest.name),
    };

    install_tls()?;
    let icon = std::fs::read(app_dir.join("icon.png")).unwrap_or_else(|_| DEFAULT_ICON.to_vec());
    let allowed_origins = dev_url
        .iter()
        .map(|url| url.origin().ascii_serialization())
        .collect();
    let assets = dev_url.is_none().then_some(plan.assets.as_path());
    let mut bridge = Bridge::with_options(
        Commands::new(),
        assets,
        dev_url,
        BridgeOptions {
            modules: Some(Box::new(move |registry, host| {
                register_all(registry, host, &context)
            })),
            allowed_origins,
            csp: Some(plan.csp),
            permissions: Some(granted),
            entry: Some(plan.manifest.windows[0].url.clone()),
            ..BridgeOptions::default()
        },
    )
    .await?;
    eprintln!(
        "ALEF_READY app={} pid={}",
        plan.manifest.id,
        std::process::id()
    );
    let handle = bridge.handle();
    // M0.5 spike: the application in a hidden WebView, no window (docs/stages/m0-spikes.md).
    let result = if std::env::var("ALEF_SPIKE_HEADLESS").is_ok_and(|value| value == "1") {
        alef_runtime::run_headless(&mut bridge)
    } else {
        alef_runtime::run(
            &mut bridge,
            WindowOptions {
                windows: plan.manifest.windows.clone(),
                ..WindowOptions::new(plan.manifest.name.clone(), icon)
                    .remembering_windows_in(window_state)
            },
        )
    };
    bridge.shutdown().await?;
    result.map_err(|error| Failure::Runtime(error.to_string()))?;
    Ok(u8::try_from(handle.exit_code()).unwrap_or(1))
}
