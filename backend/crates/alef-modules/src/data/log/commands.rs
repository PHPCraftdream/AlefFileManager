// SPDX-License-Identifier: MIT OR Apache-2.0
//! Commands for the application's bounded log and crash reports.
use std::num::NonZeroUsize;

use alef_core::{
    registry::{command::Reply, context::CallContext, dispatch::Registry},
    session::executor::BlockingExecutor,
    AlefError, ErrorCode,
};
use serde::Deserialize;
use serde_json::{Map, Value};

use super::{Level, LogStore, ReadOptions};
use crate::ModuleContext;

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct WriteArgs {
    message: String,
    #[serde(default)]
    fields: Option<Map<String, Value>>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReadArgs {
    #[serde(default = "default_limit")]
    limit: usize,
    #[serde(default)]
    level: Option<Level>,
}

fn default_limit() -> usize {
    100
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct EmptyArgs {}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct CrashArgs {
    id: String,
}

fn closed() -> AlefError {
    AlefError::new(ErrorCode::Closed, "call cancelled")
}

fn check(ctx: &CallContext) -> Result<(), AlefError> {
    if ctx.cancel().is_cancelled() || !ctx.session.is_open() {
        Err(closed())
    } else {
        Ok(())
    }
}

pub(crate) fn register(registry: &mut Registry, context: &ModuleContext) -> Result<(), AlefError> {
    let store = LogStore::new(context.paths.app_data.clone())?;
    let executor = BlockingExecutor::new(NonZeroUsize::new(8).expect("non-zero log job budget"));

    for (name, level) in [
        ("log.debug", Level::Debug),
        ("log.info", Level::Info),
        ("log.warn", Level::Warn),
        ("log.error", Level::Error),
    ] {
        let store = store.clone();
        let executor = executor.clone();
        registry
            .command::<WriteArgs>(name)?
            .handler(move |ctx, args| {
                let store = store.clone();
                let executor = executor.clone();
                async move {
                    check(&ctx)?;
                    let job = executor.submit(move || {
                        store.write(
                            level,
                            &args.message,
                            Value::Object(args.fields.unwrap_or_default()),
                        )?;
                        Ok(Reply::Json(Value::Null))
                    })?;
                    job.result().await?
                }
            })?;
    }

    let at = store.clone();
    let read_executor = executor.clone();
    registry
        .command::<ReadArgs>("log.read")?
        .handler(move |ctx, args| {
            let store = at.clone();
            let executor = read_executor.clone();
            async move {
                check(&ctx)?;
                let job = executor.submit(move || {
                    let result = store.read(ReadOptions {
                        limit: args.limit,
                        level: args.level,
                    })?;
                    crate::json(&result.records)
                })?;
                job.result().await?
            }
        })?;

    let at = store.clone();
    let crashes_executor = executor.clone();
    registry
        .command::<EmptyArgs>("log.crashes")?
        .handler(move |ctx, _args| {
            let store = at.clone();
            let executor = crashes_executor.clone();
            async move {
                check(&ctx)?;
                let job = executor.submit(move || crate::json(&store.crashes()?))?;
                job.result().await?
            }
        })?;

    registry
        .command::<CrashArgs>("log.crash")?
        .handler(move |ctx, args| {
            let store = store.clone();
            let executor = executor.clone();
            async move {
                check(&ctx)?;
                let job = executor.submit(move || crate::json(&store.crash(&args.id)?))?;
                job.result().await?
            }
        })?;

    Ok(())
}
