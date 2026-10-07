// SPDX-License-Identifier: MIT OR Apache-2.0
//! `sqlite`: databases in files. The SQLite is the program's own (bundled), the same on every system.
//! A database is a file, so it is found as every file is: the path goes through the scopes of
//! `permissions.fs` (and, where the user chose a stand-in, the database lives in the stand-in).
//! A database is a resource of the document: it closes with it. Each connection has a thread of its
//! own; what is asked of a connection runs there in its turn.
use std::{future::Future, pin::Pin, sync::Arc};

use alef_core::{
    ids::ResourceId,
    registry::{command::Reply, context::CallContext, dispatch::Registry},
    security::permissions::{refusal, Permission, Reach},
    session::Resource,
    AlefError, ErrorCode,
};
use serde::Deserialize;
use serde_json::{json, Value};
use tokio::sync::{mpsc, oneshot};

use super::fs::space::Space;
use crate::{json, ModuleContext};

mod values;
mod worker;

use values::Params;
use worker::Worker;

/// The most rows `sqlite.query` gives in one answer; more is for `iterate`.
pub const MAX_ROWS: usize = 100_000;
/// About how much one answer of `query` holds.
const MAX_WEIGHT: usize = 32 * 1024 * 1024;
/// How many rows (or about how many bytes) a frame of `iterate` carries.
const BATCH_ROWS: usize = 500;
const BATCH_WEIGHT: usize = 512 * 1024;

/// The resource a database is: it ends with the document.
struct Database {
    worker: Arc<Worker>,
}

impl Resource for Database {
    fn close(self: Box<Self>) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        Box::pin(async move {
            let worker = self.worker.clone();
            let _ = tokio::task::spawn_blocking(move || worker.shutdown()).await;
        })
    }
}

/// A statement that was prepared: its text and its database (it is run through the cache of the
/// connection, so that it is parsed once).
struct Statement {
    database: ResourceId,
    sql: String,
}

impl Resource for Statement {
    fn close(self: Box<Self>) -> Pin<Box<dyn Future<Output = ()> + Send>> {
        Box::pin(async {})
    }
}

fn invalid(message: &str) -> AlefError {
    AlefError::new(ErrorCode::InvalidArgument, message)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct OpenArgs {
    path: String,
    #[serde(default)]
    readonly: bool,
    #[serde(default = "yes")]
    create: bool,
}

fn yes() -> bool {
    true
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DatabaseArgs {
    db: u64,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct PrepareArgs {
    db: u64,
    sql: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct StatementArgs {
    statement: u64,
}

/// A statement to run: a text on a database, or a prepared statement; and its parameters.
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RunArgs {
    #[serde(default)]
    db: Option<u64>,
    #[serde(default)]
    sql: Option<String>,
    #[serde(default)]
    statement: Option<u64>,
    #[serde(default)]
    params: Option<Value>,
}

/// The connection and the text a command works on, and its parameters.
struct Run {
    worker: Arc<Worker>,
    sql: String,
    params: Params,
}

fn database(ctx: &CallContext, id: u64) -> Result<Arc<Worker>, AlefError> {
    ctx.resources()
        .with_as::<Database, _>(ResourceId(id), |database| database.worker.clone())
}

fn resolve(ctx: &CallContext, args: RunArgs) -> Result<Run, AlefError> {
    let params = Params::from_json(args.params.as_ref())?;
    match (args.db, args.sql, args.statement) {
        (Some(db), Some(sql), None) => Ok(Run {
            worker: database(ctx, db)?,
            sql,
            params,
        }),
        (None, None, Some(statement)) => {
            let (db, sql) = ctx
                .resources()
                .with_as::<Statement, _>(ResourceId(statement), |statement| {
                    (statement.database, statement.sql.clone())
                })?;
            Ok(Run {
                worker: database(ctx, db.0)?,
                sql,
                params,
            })
        }
        _ => Err(invalid(
            "name a database and the text of a statement, or a prepared statement",
        )),
    }
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, AlefError> + Send + 'static,
) -> Result<T, AlefError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|error| AlefError::new(ErrorCode::Internal, error.to_string()))?
}

pub(crate) fn register(registry: &mut Registry, context: &ModuleContext) -> Result<(), AlefError> {
    let space = Space::new(context.shadow.clone());

    registry
        .command::<OpenArgs>("sqlite.open")?
        .handler(move |ctx, args| {
            let at = space.clone();
            async move {
                // A database is read and, unless it is opened to be read only, written: both rights
                // decided the same for this path, or the file would be real for one and a stand-in
                // for the other.
                let mut place = None;
                if !args.readonly {
                    place =
                        Some(at.place(&ctx, Permission::FsWrite, &args.path, Reach::Through)?);
                }
                let seen = at.place(&ctx, Permission::FsRead, &args.path, Reach::Through)?;
                if place
                    .as_ref()
                    .is_some_and(|written| written.real != seen.real)
                {
                    return Err(refusal(Permission::FsRead));
                }
                let place = place.unwrap_or(seen);
                let (readonly, create) = (args.readonly, args.create);
                let connection = blocking(move || {
                    place.prepare().map_err(|_| {
                        AlefError::new(ErrorCode::Internal, "the stand-in cannot be made")
                    })?;
                    worker::open(&place.real, readonly, create)
                })
                .await?;
                let worker = Worker::start(connection)?;
                let id = ctx.resources().insert(Box::new(Database {
                    worker: Arc::new(worker),
                }))?;
                json(&json!({ "db": id.0 }))
            }
        })?;

    registry
        .command::<DatabaseArgs>("sqlite.close")?
        .handler(|ctx, args| async move {
            ctx.resources()
                .with_as::<Database, _>(ResourceId(args.db), |_| ())?;
            ctx.resources().take(ResourceId(args.db))?.close().await;
            Ok(Reply::Json(Value::Null))
        })?;

    registry
        .command::<RunArgs>("sqlite.exec")?
        .handler(|ctx, args| async move {
            let Run {
                worker,
                sql,
                params,
            } = resolve(&ctx, args)?;
            let done = worker
                .run(move |connection| worker::exec(connection, &sql, &params))
                .await?;
            json(&worker::executed_json(&done))
        })?;

    registry
        .command::<RunArgs>("sqlite.query")?
        .handler(|ctx, args| async move {
            let Run {
                worker,
                sql,
                params,
            } = resolve(&ctx, args)?;
            let rows = worker
                .run(move |connection| {
                    worker::query(connection, &sql, &params, MAX_ROWS, MAX_WEIGHT)
                })
                .await?;
            json(&rows)
        })?;

    registry
        .command::<RunArgs>("sqlite.iterate")?
        .handler(|ctx, args| async move {
            let Run {
                worker,
                sql,
                params,
            } = resolve(&ctx, args)?;
            let (ready, answer) = oneshot::channel();
            let (batches, mut received) = mpsc::channel::<Result<Vec<Value>, AlefError>>(2);
            worker.submit(Box::new(move |connection| {
                worker::iterate(
                    connection,
                    &sql,
                    &params,
                    BATCH_ROWS,
                    BATCH_WEIGHT,
                    ready,
                    |batch| batches.blocking_send(batch).is_ok(),
                );
            }))?;
            answer
                .await
                .map_err(|_| AlefError::new(ErrorCode::Closed, "the database is closed"))??;
            let (writer, id) = ctx.streams().open_outgoing();
            let mut stopped = worker.stopped();
            tokio::spawn(async move {
                loop {
                    let batch = tokio::select! {
                        batch = received.recv() => batch,
                        // The database is being closed: dropping the receiver frees its thread.
                        _ = stopped.wait_for(|stopped| *stopped) => return,
                    };
                    let Some(batch) = batch else { break };
                    match batch {
                        // The page closed the stream, or the document went away: dropping the
                        // receiver tells the connection to stop reading.
                        Ok(rows) => {
                            if writer.send_json(Value::Array(rows)).await.is_err() {
                                return;
                            }
                        }
                        Err(error) => {
                            writer.error(error);
                            return;
                        }
                    }
                }
                writer.end();
            });
            json(&json!({ "stream": id.0 }))
        })?;

    registry
        .command::<PrepareArgs>("sqlite.prepare")?
        .handler(|ctx, args| async move {
            let worker = database(&ctx, args.db)?;
            let sql = args.sql.clone();
            worker
                .run(move |connection| worker::check(connection, &sql))
                .await?;
            let id = ctx.resources().insert(Box::new(Statement {
                database: ResourceId(args.db),
                sql: args.sql,
            }))?;
            json(&json!({ "statement": id.0 }))
        })?;

    registry
        .command::<StatementArgs>("sqlite.finalize")?
        .handler(|ctx, args| async move {
            ctx.resources()
                .with_as::<Statement, _>(ResourceId(args.statement), |_| ())?;
            ctx.resources()
                .take(ResourceId(args.statement))?
                .close()
                .await;
            Ok(Reply::Json(Value::Null))
        })
}
