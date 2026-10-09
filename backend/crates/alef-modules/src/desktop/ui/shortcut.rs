// SPDX-License-Identifier: MIT OR Apache-2.0
//! `shortcut`: permission-checked, document-owned global registrations; no native backend here.
use std::{future::Future, pin::Pin, sync::Arc};

use alef_core::{
    ids::{ResourceId, SessionId},
    registry::{
        command::Reply,
        dispatch::Registry,
        host::Host,
        window::{
            shortcut::{ShortcutCall, ShortcutToken},
            UiCall,
        },
    },
    security::{consent::Decision, permissions::Permission},
    session::Resource,
    AlefError, ErrorCode,
};
use global_hotkey::hotkey::HotKey;
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::Mutex;

const MAX_ACCELERATOR_BYTES: usize = 256;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct RegisterArgs {
    accelerator: String,
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct UnregisterArgs {
    id: String,
}

fn invalid(message: &str) -> AlefError {
    AlefError::new(ErrorCode::InvalidArgument, message)
}

fn validate(accelerator: &str) -> Result<(), AlefError> {
    if accelerator.is_empty()
        || accelerator.len() > MAX_ACCELERATOR_BYTES
        || accelerator.chars().any(char::is_control)
    {
        return Err(invalid(
            "accelerator must be 1..256 bytes without control characters",
        ));
    }
    // Keep syntax identical to the pinned parser used by the eventual native host.
    accelerator
        .parse::<HotKey>()
        .map(|_| ())
        .map_err(|_| invalid("invalid shortcut accelerator"))
}

fn handle(owner: SessionId, id: ResourceId) -> String {
    format!("s{}:r{}", owner.0, id.0)
}

/// The resource a handle names, if the handle is exactly the one this session was given.
fn resource_id(text: &str, owner: SessionId) -> Result<ResourceId, AlefError> {
    text.split_once(":r")
        .and_then(|(_, resource)| resource.parse().ok())
        .map(ResourceId)
        .filter(|id| text == handle(owner, *id))
        .ok_or_else(|| AlefError::new(ErrorCode::NotFound, "shortcut not found"))
}

/// The shared state lets insertion failure roll back even though insert consumes its argument.
/// The async mutex serializes explicit unregister with session teardown, never across a sync lock.
#[derive(Clone)]
struct Shortcut {
    host: Arc<dyn Host>,
    window: u64,
    owner: SessionId,
    token: Arc<Mutex<Option<ShortcutToken>>>,
}

impl Shortcut {
    // Not cancel-safe: cancellation keeps the token in the table for retry/teardown.
    // The native host must make repeated unregister requests idempotent.
    async fn unregister(&self) -> Result<(), AlefError> {
        let mut token = self.token.lock().await;
        if let Some(native) = *token {
            self.host
                .ui(
                    self.window,
                    UiCall::Shortcut(ShortcutCall::Unregister {
                        owner: self.owner,
                        token: native,
                    }),
                )
                .await?;
            *token = None;
        }
        Ok(())
    }
}

impl Resource for Shortcut {
    fn close(self: Box<Self>) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        Box::pin(async move {
            // Resource teardown has no error channel. The host must make unregister idempotent.
            let _ = self.unregister().await;
        })
    }
}

pub(crate) fn register(registry: &mut Registry, host: Arc<dyn Host>) -> Result<(), AlefError> {
    registry
        .command::<RegisterArgs>("shortcut.register")?
        .permission(Permission::ShortcutGlobal, |_| None)
        .substitutes()
        .handler(move |ctx, args| {
            let host = host.clone();
            // Intentional detach on caller cancellation: the transaction must finish receiving
            // the native token and insert it or roll it back, even if transport aborts its handler.
            let transaction = tokio::spawn(async move {
                if ctx.body().is_some() {
                    return Err(invalid("shortcut.register does not accept a binary body"));
                }
                validate(&args.accelerator)?;
                let window = ctx.session.window();
                let owner = ctx.session.id();
                let token = if ctx.decision() == Decision::Substitute {
                    None
                } else {
                    let reply = host
                        .ui(
                            window,
                            UiCall::Shortcut(ShortcutCall::Register {
                                owner,
                                accelerator: args.accelerator,
                            }),
                        )
                        .await?;
                    let native = reply
                        .as_u64()
                        .filter(|token| (1..=0xBFFF).contains(token))
                        .ok_or_else(|| {
                            AlefError::new(
                                ErrorCode::Internal,
                                "invalid shortcut token from UI host",
                            )
                        })?;
                    Some(ShortcutToken(native))
                };
                let shortcut = Shortcut {
                    host,
                    window,
                    owner,
                    token: Arc::new(Mutex::new(token)),
                };
                let id = match ctx.resources().insert(Box::new(shortcut.clone())) {
                    Ok(id) => id,
                    Err(error) => {
                        shortcut.unregister().await?;
                        return Err(error);
                    }
                };
                Ok(Reply::Json(json!({
                    "id": handle(owner, id),
                    "owner": owner.0,
                    "token": token.map(|native| native.0),
                })))
            });
            async move {
                transaction.await.map_err(|_| {
                    AlefError::new(ErrorCode::Internal, "shortcut registration task failed")
                })?
            }
        })?;
    registry
        .command::<UnregisterArgs>("shortcut.unregister")?
        .handler(|ctx, args| async move {
            if ctx.body().is_some() {
                return Err(invalid("shortcut.unregister does not accept a binary body"));
            }
            let id = resource_id(&args.id, ctx.session.id())?;
            let shortcut = ctx.resources().with_as::<Shortcut, _>(id, Clone::clone)?;
            shortcut.unregister().await?;
            ctx.resources().take(id)?.close().await;
            Ok(Reply::Json(Value::Null))
        })
}
