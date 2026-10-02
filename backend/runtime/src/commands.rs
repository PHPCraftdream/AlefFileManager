// SPDX-License-Identifier: GPL-3.0-or-later
use std::collections::HashMap;
use std::future::Future;
use std::io;
use std::pin::Pin;
use std::sync::Arc;

use serde::{de::DeserializeOwned, Serialize};
use serde_json::Value;

type CommandFuture = Pin<Box<dyn Future<Output = io::Result<Value>> + Send>>;
type Command = Arc<dyn Fn(Value) -> CommandFuture + Send + Sync>;

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
        F: Fn(A) -> Fut + Send + Sync + 'static,
        Fut: Future<Output = io::Result<R>> + Send + 'static,
    {
        let name = name.into();
        if self.handlers.contains_key(&name) {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "Command already registered",
            ));
        }
        self.handlers.insert(
            name,
            Arc::new(move |arguments| {
                let arguments = serde_json::from_value(arguments)
                    .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error));
                let future = arguments.map(&handler);
                Box::pin(async move {
                    let result = future?.await?;
                    serde_json::to_value(result).map_err(io::Error::other)
                })
            }),
        );
        Ok(())
    }

    pub async fn invoke(&self, name: &str, arguments: Value) -> io::Result<Value> {
        let handler = self
            .handlers
            .get(name)
            .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "Unknown native command"))?;
        handler(arguments).await
    }
}
