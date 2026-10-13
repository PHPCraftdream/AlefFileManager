// SPDX-License-Identifier: MIT OR Apache-2.0
//! The local store of installed runtime versions: `<runtime_home()/runtimes>/<MAJOR.MINOR.PATCH>/<binary>`.
pub mod command;
pub mod install;
pub mod prune;
pub mod store;

pub use install::install;
pub use prune::prune;
pub use store::{installed, select, Installed, StoreError};

#[cfg(test)]
mod support;
