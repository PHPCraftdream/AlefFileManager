// SPDX-License-Identifier: MIT OR Apache-2.0
//! Native updater feed check.
use alef_core::{
    registry::{command::Reply, context::CallContext, dispatch::Registry},
    security::manifest::{runtime_requirement_met, Updater},
    AlefError, ErrorCode,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use std::{
    path::PathBuf,
    sync::{Arc, Mutex},
};
use url::Url;

mod client;
mod document;
mod download;
mod install;

pub use install::UpdateApplier;
mod storage;

#[cfg(test)]
mod tests;

#[derive(Clone)]
pub struct UpdaterContext {
    pub config: Updater,
    pub app_id: String,
    pub version: String,
    /// The version of the runtime binary this process runs.
    pub runtime_version: String,
    /// Versions installed in the runtime store, computed by the launcher and passed in as data;
    /// alef-modules never discovers them itself.
    pub installed_runtime_versions: Vec<String>,
    pub trusted_key: [u8; 32],
    pub data: PathBuf,
    /// The running package path when it comes from the user store of `alef install`.
    pub installed: Option<InstalledApp>,
    /// Provided by the launcher; performs the transactional install and the relaunch.
    pub applier: Arc<dyn install::UpdateApplier>,
}

/// Proves the running application was launched from the user store, not `alef run <file>`
/// or a directory; the launcher computes this when it opens the package.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct InstalledApp {
    pub package: PathBuf,
}

#[derive(Debug, Default)]
pub(super) struct State {
    pub offered: Mutex<Option<document::Document>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Args {}

#[derive(Debug, Serialize, ts_rs::TS)]
#[ts(export, export_to = "modules.ts")]
struct UpdaterCheckReply {
    available: bool,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    version: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    notes: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    runtime: Option<UpdaterRuntimeRequirement>,
}

#[derive(Debug, Serialize, ts_rs::TS)]
#[ts(export, export_to = "modules.ts")]
struct UpdaterRuntimeRequirement {
    range: String,
    satisfied: bool,
}

/// The range is satisfied by the running runtime or by any installed runtime version.
pub(super) fn runtime_satisfied(config: &UpdaterContext, range: &str) -> bool {
    runtime_requirement_met(range, &config.runtime_version)
        || config
            .installed_runtime_versions
            .iter()
            .any(|version| runtime_requirement_met(range, version))
}

pub fn register_updater(
    registry: &mut Registry,
    context: &Option<UpdaterContext>,
) -> Result<(), AlefError> {
    let ctx = context.clone();
    let state = Arc::new(State::default());
    let gate = Arc::new(tokio::sync::Semaphore::new(1));
    download::register(registry, context, state.clone(), gate.clone())?;
    install::register(registry, context, state.clone(), gate.clone())?;
    registry.command::<Args>("updater.check")?.waits().handler(
        move |_call: CallContext, _args| {
            let config = ctx.clone();
            let state = state.clone();
            let gate = gate.clone();
            async move {
                let _permit = gate.try_acquire_owned().map_err(|_| {
                    AlefError::new(ErrorCode::UpdateBusy, "updater operation already active")
                })?;
                let Some(config) = config else {
                    return Err(AlefError::new(
                        ErrorCode::NotAvailable,
                        "native updater is unavailable",
                    ));
                };
                *state.offered.lock().unwrap_or_else(|e| e.into_inner()) = None;
                let endpoint = Url::parse(&config.config.endpoint).map_err(|_| {
                    AlefError::new(ErrorCode::UpdateBadDocument, "invalid updater endpoint")
                })?;
                let bytes = client::get(&endpoint).await?;
                let parsed = document::parse(&bytes, &endpoint, &config.version)?;
                let reply = if let Some(doc) = parsed {
                    let runtime = doc.runtime.as_ref().map(|range| UpdaterRuntimeRequirement {
                        range: range.clone(),
                        satisfied: runtime_satisfied(&config, range),
                    });
                    *state.offered.lock().unwrap_or_else(|e| e.into_inner()) = Some(doc.clone());
                    UpdaterCheckReply {
                        available: true,
                        version: Some(doc.version),
                        notes: doc.notes,
                        runtime,
                    }
                } else {
                    UpdaterCheckReply {
                        available: false,
                        version: None,
                        notes: None,
                        runtime: None,
                    }
                };
                Ok(Reply::Json(json!(reply)))
            }
        },
    )?;
    Ok(())
}
