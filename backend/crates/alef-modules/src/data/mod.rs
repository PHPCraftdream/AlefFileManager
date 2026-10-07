// SPDX-License-Identifier: MIT OR Apache-2.0
//! Data modules: `fs` (files and folders); `store`, `sqlite` and `crypto` follow in the same stage.
use alef_core::{registry::dispatch::Registry, AlefError};

use crate::ModuleContext;

pub mod fs;

pub(crate) fn register(registry: &mut Registry, context: &ModuleContext) -> Result<(), AlefError> {
    fs::register(registry, context)
}
