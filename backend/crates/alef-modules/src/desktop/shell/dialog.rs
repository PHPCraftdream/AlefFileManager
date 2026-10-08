// SPDX-License-Identifier: MIT OR Apache-2.0
//! `dialog`: native dialogs. The module checks the options and hands the dialog to the process that
//! owns the windows (`Host::ui`); what the user picks becomes a grant of the session of the
//! calling document: `open` reads (a folder with everything below it), `save` writes that one
//! file. A cancelled dialog grants nothing; grants end with the session.
use std::{path::Path, sync::Arc};

use alef_core::{
    registry::{
        command::Reply,
        dialog::{ConfirmOptions, DialogCall, MessageOptions, OpenOptions, SaveOptions},
        dispatch::Registry,
        host::Host,
        window::UiCall,
    },
    AlefError, ErrorCode,
};
use serde::de::DeserializeOwned;
use serde_json::Value;

fn refuse(message: impl Into<String>) -> AlefError {
    AlefError::new(ErrorCode::InvalidArgument, message)
}

fn host_said(command: &str, error: impl std::fmt::Display) -> AlefError {
    AlefError::new(
        ErrorCode::Internal,
        format!("{command}: unexpected answer of the host: {error}"),
    )
}

/// The options of a command; `null` stands for "no options" where there is a default.
fn options_of<T: DeserializeOwned + Default>(command: &str, value: Value) -> Result<T, AlefError> {
    if value.is_null() {
        return Ok(T::default());
    }
    required_of(command, value)
}

fn required_of<T: DeserializeOwned>(command: &str, value: Value) -> Result<T, AlefError> {
    serde_json::from_value(value).map_err(|error| refuse(format!("{command}: {error}")))
}

fn checked(command: &str, result: Result<(), String>) -> Result<(), AlefError> {
    result.map_err(|reason| refuse(format!("{command}: {reason}")))
}

pub(crate) fn register(registry: &mut Registry, host: Arc<dyn Host>) -> Result<(), AlefError> {
    let opener = host.clone();
    registry
        .command::<Value>("dialog.open")?
        .handler(move |ctx, value| {
            let host = opener.clone();
            async move {
                let options: OpenOptions = options_of("dialog.open", value)?;
                checked("dialog.open", options.check())?;
                let call = UiCall::Dialog(DialogCall::Open(options));
                let reply = host.ui(ctx.session.window(), call).await?;
                let paths: Vec<String> = serde_json::from_value(reply)
                    .map_err(|error| host_said("dialog.open", error))?;
                let grants = ctx.grants();
                for path in &paths {
                    grants.grant_read(Path::new(path))?;
                }
                Ok(Reply::Json(Value::from(paths)))
            }
        })?;
    let saver = host.clone();
    registry
        .command::<Value>("dialog.save")?
        .handler(move |ctx, value| {
            let host = saver.clone();
            async move {
                let options: SaveOptions = options_of("dialog.save", value)?;
                checked("dialog.save", options.check())?;
                let call = UiCall::Dialog(DialogCall::Save(options));
                let reply = host.ui(ctx.session.window(), call).await?;
                let path: Option<String> = serde_json::from_value(reply)
                    .map_err(|error| host_said("dialog.save", error))?;
                if let Some(path) = &path {
                    ctx.grants().grant_write(Path::new(path))?;
                }
                Ok(Reply::Json(Value::from(path)))
            }
        })?;
    let messenger = host.clone();
    registry
        .command::<Value>("dialog.message")?
        .handler(move |ctx, value| {
            let host = messenger.clone();
            async move {
                let options: MessageOptions = required_of("dialog.message", value)?;
                checked("dialog.message", options.check())?;
                let call = UiCall::Dialog(DialogCall::Message(options));
                host.ui(ctx.session.window(), call).await?;
                Ok(Reply::Json(Value::Null))
            }
        })?;
    registry
        .command::<Value>("dialog.confirm")?
        .handler(move |ctx, value| {
            let host = host.clone();
            async move {
                let options: ConfirmOptions = required_of("dialog.confirm", value)?;
                checked("dialog.confirm", options.check())?;
                let call = UiCall::Dialog(DialogCall::Confirm(options));
                let reply = host.ui(ctx.session.window(), call).await?;
                let confirmed: bool = serde_json::from_value(reply)
                    .map_err(|error| host_said("dialog.confirm", error))?;
                Ok(Reply::Json(Value::Bool(confirmed)))
            }
        })
}
