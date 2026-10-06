// SPDX-License-Identifier: MIT OR Apache-2.0
//! Desktop modules.
pub mod app;
pub mod args;
pub mod dialog;
mod instance;
pub mod shell;
pub mod window;

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
    app::register(registry, host.clone(), context)?;
    dialog::register(registry, host.clone())?;
    shell::register(registry, context.backends.shell.clone())?;
    window::register(registry, host)
}
