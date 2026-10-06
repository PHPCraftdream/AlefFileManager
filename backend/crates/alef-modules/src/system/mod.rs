// SPDX-License-Identifier: MIT OR Apache-2.0
//! System modules.
pub mod clipboard;
pub mod os;
pub mod path;
pub mod screen;

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
    clipboard::register(registry, context.backends.clipboard.clone())?;
    path::register(registry, context)?;
    os::register(registry, host.clone())?;
    screen::register(registry, host)
}
