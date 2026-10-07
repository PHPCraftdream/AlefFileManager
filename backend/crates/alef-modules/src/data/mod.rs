// SPDX-License-Identifier: MIT OR Apache-2.0
//! Data modules: `fs` (files and folders), `store` (values kept between runs), `sqlite` (databases in
//! files); `crypto` follows in the same stage.
use alef_core::{registry::dispatch::Registry, AlefError};

use crate::ModuleContext;

pub mod fs;
pub mod sqlite;
pub mod store;

pub(crate) fn register(registry: &mut Registry, context: &ModuleContext) -> Result<(), AlefError> {
    fs::register(registry, context)?;
    sqlite::register(registry, context)?;
    store::register(registry, context)
}
