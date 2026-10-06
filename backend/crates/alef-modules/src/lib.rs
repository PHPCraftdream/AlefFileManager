// SPDX-License-Identifier: MIT OR Apache-2.0
//! Framework modules: desktop (`app`), system (`path`, `os`); more arrive with M2 and later.
//!
//! A module registers its commands in the registry (`<module>.<command>`) with the permission and
//! scope each one needs; the registry checks them before the handler runs. Everything the modules
//! need from the process comes through [`ModuleContext`] and the [`Host`](alef_core::registry::host::Host).
use std::{ffi::OsString, sync::Arc};

use alef_core::{
    registry::{command::Reply, dispatch::Registry, host::Host},
    security::permissions::PathVars,
    AlefError, ErrorCode,
};
use serde::{Deserialize, Serialize};

pub mod desktop;
pub mod system;

pub use desktop::args::{ArgValue, ParsedArgs};

/// Identity of the running application (`app.info`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "camelCase")]
#[ts(export, export_to = "modules.ts")]
pub struct AppInfo {
    pub id: String,
    pub name: String,
    pub version: String,
    /// Version of the Alef runtime executing the application.
    pub runtime_version: String,
}

/// The facts about the process that the modules serve.
#[derive(Debug, Clone)]
pub struct ModuleContext {
    pub app: AppInfo,
    pub paths: PathVars,
    /// The command line of the application, already parsed by its schema.
    pub args: ParsedArgs,
    /// Arguments of this process after the program name; `app.relaunch` starts it again with them.
    pub process_args: Vec<OsString>,
}

/// Registers every module of this crate.
pub fn register_all(
    registry: &mut Registry,
    host: Arc<dyn Host>,
    context: &ModuleContext,
) -> Result<(), AlefError> {
    desktop::register(registry, host.clone(), context)?;
    system::register(registry, host, context)
}

pub(crate) fn json<T: Serialize>(value: &T) -> Result<Reply, AlefError> {
    serde_json::to_value(value)
        .map(Reply::Json)
        .map_err(|error| AlefError::new(ErrorCode::Internal, error.to_string()))
}
