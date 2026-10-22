// SPDX-License-Identifier: MIT OR Apache-2.0
//! Launch logic of the generic `alef` runtime: command line, manifest, and the plan derived from it.
//! Kept apart from `main` so that it is testable without a window.
pub mod application;
#[path = "../apps/mod.rs"]
pub mod apps;
pub mod args;
pub mod consent;
pub use consent::permissions;
#[path = "runtimes/install/mod.rs"]
pub mod install;
pub mod relaunch;
pub mod runtimes;

pub use application::plan;
