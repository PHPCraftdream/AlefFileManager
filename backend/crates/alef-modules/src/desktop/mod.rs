// SPDX-License-Identifier: MIT OR Apache-2.0
//! Desktop modules.
pub mod app;
pub mod args;

use std::sync::Arc;

use alef_core::{
    registry::{dispatch::Registry, host::Host},
    AlefError,
};

use crate::ModuleContext;

pub(crate) fn register(
    registry: &mut Registry,
    host: Arc<dyn Host>,
    context: &ModuleContext,
) -> Result<(), AlefError> {
    app::register(registry, host, context)
}
