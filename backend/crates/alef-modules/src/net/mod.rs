// SPDX-License-Identifier: MIT OR Apache-2.0
//! Network modules: `http` (a client); sockets, WebSocket and servers follow in the same stage. What
//! the program reaches over the network is held against the scopes of the manifest (`permissions.net`),
//! which the CORS of a page knows nothing of.
use alef_core::{registry::dispatch::Registry, AlefError};

use crate::ModuleContext;

pub mod http;

pub(crate) fn register(registry: &mut Registry, context: &ModuleContext) -> Result<(), AlefError> {
    http::register(registry, context)
}
