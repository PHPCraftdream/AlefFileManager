// SPDX-License-Identifier: MIT OR Apache-2.0
//! `window`: the windows of the application. The module checks arguments and permissions; the
//! process that owns the windows does the work (`Host::ui`) on its UI thread.
use std::sync::Arc;

use alef_core::{
    registry::{
        command::Reply,
        dispatch::Registry,
        host::Host,
        window::{UiCall, WindowCall, WindowOp},
    },
    security::{permissions::Permission, window::WindowDef},
    AlefError, ErrorCode,
};
use serde_json::{Map, Value};

/// The body of a `window.<op>` command: the fields of the operation and an optional `label`.
type Body = Option<Map<String, Value>>;

fn refuse(message: impl Into<String>) -> AlefError {
    AlefError::new(ErrorCode::InvalidArgument, message)
}

/// The call that `window.<op>` stands for. The name of the command decides the operation, whatever
/// the body says.
pub(crate) fn call_of(op: &str, body: Body) -> Result<WindowCall, AlefError> {
    let mut body = body.unwrap_or_default();
    body.insert("op".to_owned(), Value::String(op.to_owned()));
    let call: WindowCall = serde_json::from_value(Value::Object(body))
        .map_err(|error| refuse(format!("window.{op}: {error}")))?;
    let own_only = matches!(
        call.op,
        WindowOp::CloseIntercept { .. } | WindowOp::CloseAnswer { .. }
    );
    if own_only && call.label.is_some() {
        return Err(refuse(format!(
            "window.{op} concerns the window of the calling document only"
        )));
    }
    Ok(call)
}

/// The window a `window.create` asks for.
pub(crate) fn definition_of(value: Value) -> Result<WindowDef, AlefError> {
    let definition: WindowDef =
        serde_json::from_value(value).map_err(|error| refuse(format!("window.create: {error}")))?;
    definition
        .check()
        .map_err(|(field, reason)| refuse(format!("window.create: {field} {reason}")))?;
    Ok(definition)
}

pub(crate) fn register(registry: &mut Registry, host: Arc<dyn Host>) -> Result<(), AlefError> {
    for name in WindowOp::NAMES {
        let host = host.clone();
        registry
            .command::<Body>(&format!("window.{name}"))?
            .handler(move |ctx, body| {
                let host = host.clone();
                async move {
                    let call = call_of(name, body)?;
                    let reply = host.ui(ctx.session.window(), UiCall::Window(call)).await?;
                    Ok(Reply::Json(reply))
                }
            })?;
    }
    let creator = host.clone();
    registry
        .command::<Value>("window.create")?
        .permission(Permission::WindowCreate, |_| None)
        .handler(move |ctx, value| {
            let host = creator.clone();
            async move {
                let definition = definition_of(value)?;
                let reply = host
                    .ui(ctx.session.window(), UiCall::Create(Box::new(definition)))
                    .await?;
                Ok(Reply::Json(reply))
            }
        })?;
    registry
        .command::<()>("window.all")?
        .handler(move |ctx, ()| {
            let host = host.clone();
            async move {
                let reply = host.ui(ctx.session.window(), UiCall::Windows).await?;
                Ok(Reply::Json(reply))
            }
        })
}
