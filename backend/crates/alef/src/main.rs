// SPDX-License-Identifier: MIT OR Apache-2.0
//! The generic Alef runtime: `alef --app <directory> [--dev-url <url>]`.
use std::{ffi::OsString, process::ExitCode};

use alef_core::error::AlefError;
use alef_launch::{
    args::{parse_args, Command, Launch, USAGE},
    plan::{load_manifest, make_plan, path_vars},
};
use alef_runtime::{Bridge, BridgeOptions, Commands, WindowOptions};

surfman::declare_surfman!();

/// The default window icon (the Alef logo) unless the application ships `icon.png`.
const DEFAULT_ICON: &[u8] = include_bytes!("../../../../frontend/public/logo-32x32.png");

/// Exit codes: 2 — the command line or the manifest is unusable, 1 — the runtime failed.
enum Failure {
    Usage(String),
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
        Ok(()) => ExitCode::SUCCESS,
        Err(Failure::Usage(message)) => {
            eprintln!("alef: {message}");
            ExitCode::from(2)
        }
        Err(Failure::Runtime(message)) => {
            eprintln!("alef: {message}");
            ExitCode::from(1)
        }
    }
}

async fn launch(arguments: Vec<OsString>) -> Result<(), Failure> {
    let launch = match parse_args(arguments).map_err(Failure::Usage)? {
        Command::Help => {
            println!("{USAGE}");
            return Ok(());
        }
        Command::Run(launch) => launch,
    };
    let Launch { app_dir, dev_url } = launch;
    let manifest = load_manifest(&app_dir)?;
    let vars = path_vars(&app_dir, &manifest.id)?;
    let plan = make_plan(&app_dir, manifest, &vars)?;

    rustls::crypto::aws_lc_rs::default_provider()
        .install_default()
        .map_err(|_| Failure::Runtime("Failed to configure the Servo TLS provider".to_owned()))?;
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
            allowed_origins,
            csp: Some(plan.csp),
            permissions: Some(plan.permissions),
            entry: Some(plan.window.entry),
            ..BridgeOptions::default()
        },
    )
    .await?;
    eprintln!(
        "ALEF_READY app={} pid={}",
        plan.manifest.id,
        std::process::id()
    );
    let result = alef_runtime::run(
        &mut bridge,
        WindowOptions {
            width: plan.window.width,
            height: plan.window.height,
            min_size: plan.window.min_size,
            max_size: plan.window.max_size,
            ..WindowOptions::new(plan.manifest.name.clone(), icon)
        },
    );
    bridge.shutdown().await?;
    result.map_err(|error| Failure::Runtime(error.to_string()))
}
