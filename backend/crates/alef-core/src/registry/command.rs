// SPDX-License-Identifier: MIT OR Apache-2.0
//! Typed command entry construction.
use super::{context::CallContext, dispatch::Command};
use crate::{
    ids::StreamId,
    security::{
        consent::Decision,
        permissions::{refusal, Permission},
    },
    AlefError, ErrorCode,
};
use bytes::Bytes;
use serde::de::DeserializeOwned;
use std::{future::Future, sync::Arc};

type ScopeTarget<A> = Box<dyn Fn(&A) -> Option<String> + Send + Sync>;

/// Reply payload produced by a command handler.
#[derive(Debug)]
pub enum Reply {
    Json(serde_json::Value),
    Bytes(Bytes),
    Stream(StreamId),
}
/// Builder returned by `Registry::command`; completes registration on `CommandBuilder::handler`.
pub struct CommandBuilder<'r, A> {
    pub(super) registry: &'r mut super::dispatch::Registry,
    pub(super) name: String,
    pub(super) permission: Permission,
    pub(super) target: ScopeTarget<A>,
    pub(super) substitutes: bool,
    pub(super) waits: bool,
    pub(super) marker: std::marker::PhantomData<A>,
}
impl<'r, A: DeserializeOwned + Send + 'static> CommandBuilder<'r, A> {
    /// Declare the permission and argument-derived scope target required for dispatch.
    pub fn permission(
        mut self,
        permission: Permission,
        target: impl Fn(&A) -> Option<String> + Send + Sync + 'static,
    ) -> Self {
        self.permission = permission;
        self.target = Box::new(target);
        self
    }
    /// The handler gives a stand-in when [`CallContext::decision`] is `Substitute`. A command that
    /// does not say so is refused such a decision as a denial: no handler hands out the real thing
    /// to a user who chose a stand-in.
    pub fn substitutes(mut self) -> Self {
        self.substitutes = true;
        self
    }
    /// The handler waits on something outside (the network, a process): the call leaves the budget
    /// of running calls for that of waiting ones before the handler starts. A substituted call
    /// does the same, since a stand-in waits as long as the real thing would.
    pub fn waits(mut self) -> Self {
        self.waits = true;
        self
    }
    /// Register the async handler; handler errors pass through unchanged.
    pub fn handler<F, Fut>(self, handler: F) -> Result<(), AlefError>
    where
        F: Fn(CallContext, A) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = Result<Reply, AlefError>> + Send + 'static,
    {
        let permission = self.permission;
        let substitutes = self.substitutes;
        let waits = self.waits;
        let target = Arc::new(self.target);
        let handler = Arc::new(handler);
        let invoke = Arc::new(
            move |ctx: CallContext, value: serde_json::Value| -> super::dispatch::Invocation {
                let handler = Arc::clone(&handler);
                let target = Arc::clone(&target);
                // The permission check is awaited: filesystem scopes resolve off the poll.
                Box::pin(async move {
                    let args: A = serde_json::from_value(value).map_err(|_| {
                        AlefError::new(ErrorCode::InvalidArgument, "invalid command arguments")
                    })?;
                    let scope = target(&args);
                    let decision = ctx
                        .permissions
                        .clone()
                        .check_async(permission, scope.as_deref(), &ctx.grants())
                        .await?;
                    if decision == Decision::Substitute && !substitutes {
                        return Err(refusal(permission));
                    }
                    if waits || decision == Decision::Substitute {
                        ctx.waiting()?;
                    }
                    handler(ctx.with_decision(decision), args).await
                })
            },
        );
        self.registry.insert(
            self.name,
            Arc::new(Command {
                invoke: Box::new(move |ctx: CallContext, value: serde_json::Value| {
                    let invoke = invoke.clone();
                    invoke(ctx, value)
                }),
            }),
        )
    }
}
