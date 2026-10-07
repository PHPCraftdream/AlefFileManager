// SPDX-License-Identifier: MIT OR Apache-2.0
//! Data modules: `fs` (files and folders), `store` (values kept between runs), `sqlite` (databases in
//! files), `secrets` (the credential store of the system), `crypto` (what the engine does not give a page:
//! random bytes, hashes, keys, ciphers).
use alef_core::{registry::dispatch::Registry, AlefError};

use crate::ModuleContext;

pub mod crypto;
pub mod fs;
pub mod secrets;
pub mod sqlite;
pub mod store;

pub(crate) fn register(registry: &mut Registry, context: &ModuleContext) -> Result<(), AlefError> {
    crypto::register(registry)?;
    fs::register(registry, context)?;
    secrets::register(registry, context)?;
    sqlite::register(registry, context)?;
    store::register(registry, context)
}
