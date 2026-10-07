// SPDX-License-Identifier: MIT OR Apache-2.0
//! Concurrent command registration and asynchronous dispatch.
//! Dispatch is async because handlers are async; blocking the executor with `block_on` could deadlock under load.
use super::{
    command::{CommandBuilder, Reply},
    context::CallContext,
};
use crate::security::permissions::Permission;
use crate::{AlefError, ErrorCode};
use serde::de::DeserializeOwned;
use std::{
    collections::HashMap,
    future::Future,
    pin::Pin,
    sync::{Arc, RwLock},
};

/// Callable future returned from a command entry.
pub(super) type Invocation = Pin<Box<dyn Future<Output = Result<Reply, AlefError>> + Send>>;
/// Type-erased handler used by the registry.
pub(super) type InvokeFn = dyn Fn(CallContext, serde_json::Value) -> Invocation + Send + Sync;
pub(super) struct Command {
    pub(super) invoke: Box<InvokeFn>,
}
/// Command registry; cloneable, concurrency-safe for registration and dispatch.
/// Async dispatch avoids blocking the executor with `block_on`, which could deadlock under load.
#[derive(Clone, Default)]
pub struct Registry {
    pub(super) commands: Arc<RwLock<HashMap<String, Arc<Command>>>>,
}
impl Registry {
    /// Start registering a valid command name; `runtime.` is reserved.
    pub fn command<A: DeserializeOwned + Send + 'static>(
        &mut self,
        name: &str,
    ) -> Result<CommandBuilder<'_, A>, AlefError> {
        self.builder(name, false)
    }
    /// Start registration in the reserved runtime namespace.
    pub fn register_runtime<A: DeserializeOwned + Send + 'static>(
        &mut self,
        name: &str,
    ) -> Result<CommandBuilder<'_, A>, AlefError> {
        self.builder(name, true)
    }
    fn builder<A: DeserializeOwned + Send + 'static>(
        &mut self,
        name: &str,
        runtime: bool,
    ) -> Result<CommandBuilder<'_, A>, AlefError> {
        if !valid_name(name) {
            return Err(AlefError::new(
                ErrorCode::InvalidArgument,
                "invalid command name",
            ));
        }
        if name.starts_with("runtime.") && !runtime {
            return Err(AlefError::new(
                ErrorCode::InvalidArgument,
                "reserved runtime namespace",
            ));
        }
        if self
            .commands
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .contains_key(name)
        {
            return Err(AlefError::new(
                ErrorCode::AlreadyExists,
                "command already registered",
            ));
        }
        Ok(CommandBuilder {
            registry: self,
            name: name.to_owned(),
            permission: Permission::None,
            target: Box::new(|_| None),
            substitutes: false,
            marker: std::marker::PhantomData,
        })
    }
    pub(super) fn insert(&mut self, name: String, command: Arc<Command>) -> Result<(), AlefError> {
        let mut entries = self.commands.write().unwrap_or_else(|e| e.into_inner());
        if entries.contains_key(&name) {
            return Err(AlefError::new(
                ErrorCode::AlreadyExists,
                "command already registered",
            ));
        }
        entries.insert(name, command);
        Ok(())
    }
    /// Returns all registered command names, sorted.
    pub fn command_names(&self) -> Vec<String> {
        let mut names = self
            .commands
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .keys()
            .cloned()
            .collect::<Vec<_>>();
        names.sort();
        names
    }
    /// Dispatch asynchronously; checks authorization before the handler, drops the registry lock before awaiting, and propagates handler panics.
    /// Unknown names yield `NotFound`; malformed arguments yield `InvalidArgument`; denied scopes yield `PermissionDenied`.
    pub async fn dispatch(
        &self,
        name: &str,
        ctx: CallContext,
        args: serde_json::Value,
    ) -> Result<Reply, AlefError> {
        let command = self
            .commands
            .read()
            .unwrap_or_else(|e| e.into_inner())
            .get(name)
            .cloned()
            .ok_or_else(|| AlefError::new(ErrorCode::NotFound, "command not found"))?;
        (command.invoke)(ctx, args).await
    }
}
/// Command-name rule shared with transport route validation.
pub fn valid_name(name: &str) -> bool {
    let parts = name.split('.').collect::<Vec<_>>();
    parts.len() >= 2
        && parts.iter().all(|p| {
            let mut b = p.bytes();
            matches!(b.next(),Some(c)if c.is_ascii_lowercase())
                && b.all(|c| c.is_ascii_alphanumeric())
        })
}
