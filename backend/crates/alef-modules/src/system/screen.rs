// SPDX-License-Identifier: MIT OR Apache-2.0
//! `screen`: the displays and the cursor. Read from the thread that owns the windows.
use std::sync::Arc;

use alef_core::{
    registry::{command::Reply, dispatch::Registry, host::Host, window::UiCall},
    AlefError,
};

pub(crate) fn register(registry: &mut Registry, host: Arc<dyn Host>) -> Result<(), AlefError> {
    let displays = host.clone();
    registry
        .command::<()>("screen.monitors")?
        .handler(move |ctx, ()| {
            let host = displays.clone();
            async move {
                let reply = host.ui(ctx.session.window(), UiCall::Monitors).await?;
                Ok(Reply::Json(reply))
            }
        })?;
    registry
        .command::<()>("screen.cursorPosition")?
        .handler(move |ctx, ()| {
            let host = host.clone();
            async move {
                let reply = host
                    .ui(ctx.session.window(), UiCall::CursorPosition)
                    .await?;
                Ok(Reply::Json(reply))
            }
        })
}
