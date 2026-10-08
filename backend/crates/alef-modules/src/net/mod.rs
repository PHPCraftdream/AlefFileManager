// SPDX-License-Identifier: MIT OR Apache-2.0
//! Network modules: `http` (a client) and `socket`; WebSocket and servers follow in the same stage. What
//! the program reaches over the network is held against the scopes of the manifest (`permissions.net`),
//! which the CORS of a page knows nothing of.
use alef_core::{registry::dispatch::Registry, AlefError};

use crate::ModuleContext;

mod headers;
pub mod http;
pub mod socket;
pub mod websocket;

pub(crate) fn register(registry: &mut Registry, context: &ModuleContext) -> Result<(), AlefError> {
    http::register(registry, context)?;
    socket::register(registry)?;
    websocket::register(registry)
}
