// SPDX-License-Identifier: MIT OR Apache-2.0
//! The generic Alef runtime: `alef --app <directory> [--dev-url <url>]`.
use std::{ffi::OsString, process::ExitCode};

mod cli;

use alef_launch::relaunch;
use alef_runtime::mark;
use cli::{dispatch, failure::Failure};

surfman::declare_surfman!();

fn main() -> ExitCode {
    mark("main");
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
    dispatch::warn_end_to_end_build();
    let arguments: Vec<OsString> = std::env::args_os().skip(1).collect();
    match dispatch::launch(arguments).await {
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
