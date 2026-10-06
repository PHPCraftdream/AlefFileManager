// SPDX-License-Identifier: MIT OR Apache-2.0
//! The application's command table. The same handlers are reachable through the legacy
//! `native://invoke` route (removed in M1.6) and through the registry behind `native://call`.
use std::collections::HashMap;
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::Arc;
use std::time::Duration;

use alef_core::{
    error::AlefError,
    registry::{
        command::Reply,
        dispatch::{valid_name, Registry},
    },
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::Value;

use super::transport::{read_body, OwnedTask};
use super::{authorize, MemoryProtocol};
use crate::{RuntimeHandle, WindowAction};

type CommandFuture = Pin<Box<dyn Future<Output = io::Result<Value>> + Send>>;
type Command = Arc<dyn Fn(Value, RuntimeHandle) -> CommandFuture + Send + Sync>;

const MAX_REQUEST_BYTES: usize = 256 * 1024;

#[derive(Clone, Default)]
pub struct Commands {
    handlers: HashMap<String, Command>,
}

impl Commands {
    pub fn new() -> Self {
        Self::default()
    }

    pub fn register<A, R, F, Fut>(&mut self, name: impl Into<String>, handler: F) -> io::Result<()>
    where
        A: DeserializeOwned + Send + 'static,
        R: Serialize + Send + 'static,
        F: Fn(A, RuntimeHandle) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = io::Result<R>> + Send + 'static,
    {
        let name = name.into();
        if name.starts_with("runtime.") {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "The runtime command namespace is reserved",
            ));
        }
        if self.handlers.contains_key(&name) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "Command already registered",
            ));
        }
        self.handlers.insert(
            name,
            Arc::new(move |arguments, context| {
                let arguments = serde_json::from_value(arguments)
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error));
                let future = arguments.map(|arguments| handler(arguments, context));
                Box::pin(async move {
                    let result = future?.await?;
                    serde_json::to_value(result).map_err(io::Error::other)
                })
            }),
        );
        Ok(())
    }

    pub async fn invoke(
        &self,
        name: &str,
        arguments: Value,
        context: RuntimeHandle,
    ) -> io::Result<Value> {
        let handler = self
            .handlers
            .get(name)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "Unknown native command"))?;
        handler(arguments, context).await
    }

    /// Name of a legacy command on the registry: `module.command` names are kept, a bare name
    /// such as `hello` lives under `app.` (`app.hello`).
    pub(crate) fn registry_name(name: &str) -> String {
        if valid_name(name) {
            name.to_owned()
        } else {
            format!("app.{name}")
        }
    }

    /// Builds the registry behind `native://call`: every legacy command under its registry name,
    /// plus the interim `window.apply` (same JSON as the legacy `runtime.window`; replaced by the
    /// window module in M2.2).
    pub(crate) fn to_registry(&self, ui: &RuntimeHandle) -> Result<Registry, AlefError> {
        let mut registry = Registry::default();
        for (name, handler) in &self.handlers {
            let handler = handler.clone();
            let ui = ui.clone();
            registry
                .command::<Value>(&Self::registry_name(name))?
                .handler(move |_ctx, arguments| {
                    let (handler, ui) = (handler.clone(), ui.clone());
                    async move {
                        let value = handler(arguments, ui).await?;
                        Ok(Reply::Json(value))
                    }
                })?;
        }
        let ui = ui.clone();
        registry
            .command::<WindowAction>("window.apply")?
            .handler(move |_ctx, action| {
                let ui = ui.clone();
                async move { Ok(Reply::Json(ui.window(action).await?)) }
            })?;
        Ok(registry)
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct Invocation {
    command: String,
    arguments: Value,
}

impl MemoryProtocol {
    /// Legacy `POST native://invoke/` — removed in M1.6 together with the old frontend client.
    pub(super) async fn invoke(
        &self,
        request: &mut servo::protocol_handler::Request,
    ) -> io::Result<Value> {
        if request.method == http::Method::OPTIONS {
            return Ok(Value::Null);
        }
        if request.method != http::Method::POST {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Invoke requires POST",
            ));
        }
        authorize(&self.token, &request.headers)?;
        let permit = self.admission.clone().try_acquire_owned().map_err(|_| {
            io::Error::new(io::ErrorKind::WouldBlock, "Native command capacity reached")
        })?;
        let body = request.body.take().ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidInput, "Missing invocation body")
        })?;
        let commands = self.commands.clone();
        let ui = self.ui.clone();
        let mut task = OwnedTask(self.handle.spawn(async move {
            let _permit = permit;
            let bytes =
                tokio::time::timeout(Duration::from_secs(10), read_body(body, MAX_REQUEST_BYTES))
                    .await
                    .map_err(|_| {
                        io::Error::new(io::ErrorKind::TimedOut, "Invocation body timed out")
                    })??;
            let invocation: Invocation = serde_json::from_slice(&bytes)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
            if invocation.command == "runtime.window" {
                let action: WindowAction = serde_json::from_value(invocation.arguments)
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
                ui.window(action).await
            } else {
                commands
                    .invoke(&invocation.command, invocation.arguments, ui)
                    .await
            }
        }));
        (&mut task.0).await.map_err(io::Error::other)?
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::bridge::testing;

    #[test]
    fn application_cannot_shadow_window_commands() {
        let mut commands = Commands::new();
        let error = commands
            .register("runtime.window", |(): (), _context| async { Ok(()) })
            .expect_err("reserved namespace");
        assert_eq!(error.kind(), io::ErrorKind::InvalidInput);
    }

    #[test]
    fn bare_legacy_names_move_under_app_and_namespaced_ones_stay() {
        assert_eq!(Commands::registry_name("hello"), "app.hello");
        assert_eq!(Commands::registry_name("directory.list"), "directory.list");
        assert_eq!(
            Commands::registry_name("preferences.get"),
            "preferences.get"
        );
    }

    #[tokio::test]
    async fn legacy_handlers_are_reachable_through_the_registry_under_their_registry_names() {
        use alef_core::registry::context::CallContext;
        let mut commands = Commands::new();
        commands
            .register("hello", |name: String, _ui| async move {
                Ok(format!("hi {name}"))
            })
            .expect("register");
        commands
            .register("math.double", |n: i64, _ui| async move { Ok(n * 2) })
            .expect("register");
        let registry = testing::registry_of(&commands);
        let names = registry.command_names();
        assert_eq!(names, vec!["app.hello", "math.double", "window.apply"]);
        let session = testing::session(1).await;
        let permissions = testing::permissions();
        let ctx = || CallContext::new(session.clone(), permissions.clone());
        let hello = registry
            .dispatch("app.hello", ctx(), serde_json::json!("there"))
            .await
            .expect("dispatch");
        assert!(
            matches!(hello, Reply::Json(ref v) if v == "hi there"),
            "{hello:?}"
        );
        let double = registry
            .dispatch("math.double", ctx(), serde_json::json!(21))
            .await
            .expect("dispatch");
        assert!(
            matches!(double, Reply::Json(ref v) if *v == 42),
            "{double:?}"
        );
        let bad = registry
            .dispatch("math.double", ctx(), serde_json::json!("not a number"))
            .await
            .expect_err("bad arguments");
        assert_eq!(bad.code, alef_core::ErrorCode::InvalidArgument);
    }

    #[tokio::test]
    async fn window_apply_without_an_attached_window_reports_the_ui_error() {
        use alef_core::registry::context::CallContext;
        let commands = Commands::new();
        let (ui, _requests) = RuntimeHandle::channel();
        let registry = commands.to_registry(&ui).expect("registry");
        let session = testing::session(1).await;
        let ctx = CallContext::new(session, testing::permissions());
        let error = registry
            .dispatch(
                "window.apply",
                ctx,
                serde_json::json!({"action": "getState"}),
            )
            .await
            .expect_err("no window attached");
        assert_eq!(
            error.code,
            alef_core::ErrorCode::Closed,
            "an unattached window reads as closed"
        );
    }
}
