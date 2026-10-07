// SPDX-License-Identifier: MIT OR Apache-2.0
//! A connection lives on a thread of its own, because SQLite is synchronous: what is asked of it is a
//! closure that goes through a channel and runs in its turn, one at a time, and the answer comes back
//! on a `oneshot`. The connection is opened here with what makes it safe to give to an application:
//! it cannot attach another file (that would be a path no scope looked at), and cannot load code.
use std::{
    path::Path,
    sync::{mpsc, Mutex},
    thread::JoinHandle,
    time::Duration,
};

use alef_core::{AlefError, ErrorCode};
use rusqlite::{
    config::DbConfig,
    hooks::{AuthAction, AuthContext, Authorization},
    Connection, OpenFlags,
};
use serde_json::{json, Value};
use tokio::sync::{oneshot, watch};

use super::values::{integer, row_to_json, weight, Params};

pub(super) type Job = Box<dyn FnOnce(&mut Connection) + Send>;

/// How long a statement waits for a lock another connection holds before it says `BUSY`.
const BUSY_WAIT: Duration = Duration::from_secs(5);
/// Statements kept prepared for the next time.
const CACHED_STATEMENTS: usize = 64;

pub(super) struct Worker {
    sender: Mutex<Option<mpsc::Sender<Job>>>,
    thread: Mutex<Option<JoinHandle<()>>>,
    /// Tells whoever feeds a stream from this connection that the connection is going away.
    stopped: watch::Sender<bool>,
}

fn closed() -> AlefError {
    AlefError::new(ErrorCode::Closed, "the database is closed")
}

fn invalid(message: &str) -> AlefError {
    AlefError::new(ErrorCode::InvalidArgument, message)
}

impl Worker {
    /// Puts the connection on its thread.
    pub(super) fn start(connection: Connection) -> Result<Self, AlefError> {
        let (sender, jobs) = mpsc::channel::<Job>();
        let thread = std::thread::Builder::new()
            .name("alef-sqlite".into())
            .spawn(move || {
                let mut connection = connection;
                for job in jobs {
                    job(&mut connection);
                }
                // The channel is closed: the connection closes with the thread.
            })
            .map_err(|_| AlefError::new(ErrorCode::Internal, "the database cannot be started"))?;
        Ok(Self {
            sender: Mutex::new(Some(sender)),
            thread: Mutex::new(Some(thread)),
            stopped: watch::channel(false).0,
        })
    }

    /// Hands a job to the thread; it runs after the ones before it.
    pub(super) fn submit(&self, job: Job) -> Result<(), AlefError> {
        let sender = self.sender.lock().unwrap_or_else(|e| e.into_inner());
        sender
            .as_ref()
            .ok_or_else(closed)?
            .send(job)
            .map_err(|_| closed())
    }

    /// Runs `work` on the thread and waits for what it gives.
    pub(super) async fn run<T: Send + 'static>(
        &self,
        work: impl FnOnce(&mut Connection) -> Result<T, AlefError> + Send + 'static,
    ) -> Result<T, AlefError> {
        let (reply, answer) = oneshot::channel();
        self.submit(Box::new(move |connection| {
            let _ = reply.send(work(connection));
        }))?;
        answer.await.map_err(|_| closed())?
    }

    /// Becomes `true` when the connection is being closed.
    pub(super) fn stopped(&self) -> watch::Receiver<bool> {
        self.stopped.subscribe()
    }

    /// Ends the thread once the jobs before this are done and closes the connection; blocks until then.
    pub(super) fn shutdown(&self) {
        // An iteration that waits for its reader would never end otherwise.
        self.stopped.send_replace(true);
        self.sender.lock().unwrap_or_else(|e| e.into_inner()).take();
        let thread = self.thread.lock().unwrap_or_else(|e| e.into_inner()).take();
        if let Some(thread) = thread {
            let _ = thread.join();
        }
    }
}

/// What an application may not do whatever it writes: attach another file, or load code. (A plain
/// VACUUM attaches a database with no file, `ATTACH ''`, to copy into: that is let through.)
fn guard(context: AuthContext<'_>) -> Authorization {
    match context.action {
        AuthAction::Attach { filename } if !filename.is_empty() => Authorization::Deny,
        AuthAction::Function {
            function_name: "load_extension",
        } => Authorization::Deny,
        _ => Authorization::Allow,
    }
}

/// Opens the database at `path` (a path a scope allowed).
pub(super) fn open(path: &Path, readonly: bool, create: bool) -> Result<Connection, AlefError> {
    let exists = path.exists();
    if path.is_dir() {
        return Err(AlefError::new(ErrorCode::IsADirectory, "it is a folder"));
    }
    if !exists && (readonly || !create) {
        return Err(AlefError::new(ErrorCode::NotFound, "no such database"));
    }
    if !exists && !path.parent().is_some_and(Path::is_dir) {
        return Err(AlefError::new(ErrorCode::NotFound, "no such folder"));
    }
    let access = if readonly {
        OpenFlags::SQLITE_OPEN_READ_ONLY
    } else if create {
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE
    } else {
        OpenFlags::SQLITE_OPEN_READ_WRITE
    };
    // No URI flag: the path is a path, whatever it looks like.
    let connection = Connection::open_with_flags(path, access | OpenFlags::SQLITE_OPEN_NO_MUTEX)
        .map_err(|error| match sql_error(error) {
            refused if refused.code == ErrorCode::Internal => {
                AlefError::new(ErrorCode::PermissionDenied, "the database cannot be opened")
            }
            other => other,
        })?;
    connection.busy_timeout(BUSY_WAIT).map_err(sql_error)?;
    connection.set_prepared_statement_cache_capacity(CACHED_STATEMENTS);
    let _ = connection.set_db_config(DbConfig::SQLITE_DBCONFIG_DEFENSIVE, true);
    connection.authorizer(Some(guard)).map_err(sql_error)?;
    // A file that is not a database is told now, not at the first statement.
    connection
        .pragma_query_value(None, "schema_version", |row| row.get::<_, i64>(0))
        .map_err(sql_error)?;
    Ok(connection)
}

/// The name of the extended result codes an application is likely to want to tell apart.
fn extended_name(code: i32) -> Value {
    match code {
        275 => json!("SQLITE_CONSTRAINT_CHECK"),
        787 => json!("SQLITE_CONSTRAINT_FOREIGNKEY"),
        1299 => json!("SQLITE_CONSTRAINT_NOTNULL"),
        1555 => json!("SQLITE_CONSTRAINT_PRIMARYKEY"),
        2067 => json!("SQLITE_CONSTRAINT_UNIQUE"),
        other => json!(other),
    }
}

/// What SQLite said, in the vocabulary of `AlefError`. Its message about the statement (a syntax
/// error, a constraint that failed) is the application's own matter and is passed on; what is about
/// the machine is put in words of ours.
pub(super) fn sql_error(error: rusqlite::Error) -> AlefError {
    use rusqlite::{Error, ErrorCode as Sq};
    match error {
        Error::SqliteFailure(failure, message) => {
            let said = message.unwrap_or_else(|| failure.to_string());
            let (code, text) = match failure.code {
                Sq::DatabaseBusy | Sq::DatabaseLocked => {
                    (ErrorCode::Busy, "the database is busy".to_owned())
                }
                Sq::ReadOnly => (
                    ErrorCode::PermissionDenied,
                    "the database is read-only".to_owned(),
                ),
                Sq::AuthorizationForStatementDenied => (
                    ErrorCode::PermissionDenied,
                    "this statement is not allowed".to_owned(),
                ),
                Sq::NotADatabase => (
                    ErrorCode::InvalidArgument,
                    "not an SQLite database".to_owned(),
                ),
                Sq::DiskFull => (ErrorCode::Internal, "the disk is full".to_owned()),
                Sq::ConstraintViolation | Sq::Unknown => (ErrorCode::InvalidArgument, said),
                _ => (ErrorCode::Internal, "the database failed".to_owned()),
            };
            AlefError::new(code, text)
                .with_details(json!({ "sqlite": extended_name(failure.extended_code) }))
        }
        Error::SqlInputError { error, msg, .. } => {
            sql_error(Error::SqliteFailure(error, Some(msg)))
        }
        Error::MultipleStatement => invalid("one statement at a time here"),
        Error::InvalidParameterName(_) => invalid("no parameter has this name"),
        Error::InvalidParameterCount(..) => {
            invalid("the number of parameters is not the statement's")
        }
        Error::ExecuteReturnedResults => invalid("this statement returns rows: ask with query"),
        Error::InvalidQuery => invalid("this statement is not a query"),
        _ => AlefError::new(ErrorCode::Internal, "the database failed"),
    }
}

pub(super) struct Executed {
    pub changes: u64,
    pub last_insert_id: i64,
}

/// Runs statements that change things. With no parameters the text may hold several statements.
pub(super) fn exec(
    connection: &Connection,
    sql: &str,
    params: &Params,
) -> Result<Executed, AlefError> {
    let before = connection.total_changes();
    if params.is_none() {
        connection.execute_batch(sql).map_err(sql_error)?;
    } else {
        let mut statement = connection.prepare_cached(sql).map_err(sql_error)?;
        params.execute(&mut statement).map_err(sql_error)?;
    }
    // `changes` keeps the number of the last statement that changed something: only for this run.
    let changes = if connection.total_changes() == before {
        0
    } else {
        connection.changes()
    };
    Ok(Executed {
        changes,
        last_insert_id: connection.last_insert_rowid(),
    })
}

pub(super) fn executed_json(done: &Executed) -> Value {
    json!({ "changes": done.changes, "lastInsertId": integer(done.last_insert_id) })
}

/// Runs a query and gives its rows as objects: up to `max_rows` and about `max_weight` bytes.
pub(super) fn query(
    connection: &Connection,
    sql: &str,
    params: &Params,
    max_rows: usize,
    max_weight: usize,
) -> Result<Vec<Value>, AlefError> {
    let mut statement = connection.prepare_cached(sql).map_err(sql_error)?;
    if statement.column_count() == 0 {
        return Err(invalid("this statement returns no rows: run it with exec"));
    }
    let names: Vec<String> = statement
        .column_names()
        .iter()
        .map(|n| (*n).to_owned())
        .collect();
    let mut rows = params.query(&mut statement).map_err(sql_error)?;
    let mut all = Vec::new();
    let mut total = 0;
    while let Some(row) = rows.next().map_err(sql_error)? {
        let row = row_to_json(&names, row).map_err(sql_error)?;
        total += weight(&row);
        if all.len() == max_rows || total > max_weight {
            return Err(invalid("too many rows for one answer: iterate over them"));
        }
        all.push(row);
    }
    Ok(all)
}

/// Checks that `sql` is one statement that can be prepared.
pub(super) fn check(connection: &Connection, sql: &str) -> Result<(), AlefError> {
    connection.prepare_cached(sql).map(drop).map_err(sql_error)
}

/// The answer to a call that started an iteration: told once, and only the first telling counts.
type Ready = Option<oneshot::Sender<Result<(), AlefError>>>;

fn tell(ready: &mut Ready, outcome: Result<(), AlefError>) -> bool {
    ready
        .take()
        .is_none_or(|sender| sender.send(outcome).is_ok())
}

/// Runs a query and hands its rows over in batches (`send` says whether anybody still wants them).
/// `ready` hears whether the statement works as soon as the first row is there (or the end, or an
/// error): what goes wrong before that is the failure of the call, what goes wrong later is the
/// failure of the stream.
pub(super) fn iterate(
    connection: &Connection,
    sql: &str,
    params: &Params,
    batch_rows: usize,
    batch_weight: usize,
    ready: oneshot::Sender<Result<(), AlefError>>,
    mut send: impl FnMut(Result<Vec<Value>, AlefError>) -> bool,
) {
    let mut ready = Some(ready);
    let mut statement = match connection.prepare_cached(sql).map_err(sql_error) {
        Ok(statement) => statement,
        Err(error) => {
            tell(&mut ready, Err(error));
            return;
        }
    };
    if statement.column_count() == 0 {
        tell(
            &mut ready,
            Err(invalid("this statement returns no rows: run it with exec")),
        );
        return;
    }
    let names: Vec<String> = statement
        .column_names()
        .iter()
        .map(|name| (*name).to_owned())
        .collect();
    let mut rows = match params.query(&mut statement).map_err(sql_error) {
        Ok(rows) => rows,
        Err(error) => {
            tell(&mut ready, Err(error));
            return;
        }
    };
    let mut batch = Vec::new();
    let mut total = 0;
    loop {
        let next = match rows.next() {
            Ok(Some(row)) => row_to_json(&names, row).map(Some),
            Ok(None) => Ok(None),
            Err(error) => Err(error),
        };
        let row = match next.map_err(sql_error) {
            Ok(Some(row)) => row,
            Ok(None) => break,
            Err(error) => {
                if ready.is_some() {
                    tell(&mut ready, Err(error));
                } else {
                    // What was read before the error reaches the reader first.
                    if !batch.is_empty() && !send(Ok(std::mem::take(&mut batch))) {
                        return;
                    }
                    send(Err(error));
                }
                return;
            }
        };
        if !tell(&mut ready, Ok(())) {
            return;
        }
        total += weight(&row);
        batch.push(row);
        if (batch.len() >= batch_rows || total >= batch_weight)
            && !send(Ok(std::mem::take(&mut batch)))
        {
            return;
        }
        if batch.is_empty() {
            total = 0;
        }
    }
    if !tell(&mut ready, Ok(())) {
        return;
    }
    if !batch.is_empty() {
        send(Ok(batch));
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn memory() -> Connection {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch(
                "CREATE TABLE t (n INTEGER, s TEXT); INSERT INTO t VALUES (1, 'a'), (2, 'b'), (3, 'c');",
            )
            .unwrap();
        connection
    }

    type Told = Vec<Result<Vec<Value>, AlefError>>;

    /// What `iterate` told: the answer to the call, and every batch (or error) in order.
    fn iterated(
        connection: &Connection,
        sql: &str,
        batch_rows: usize,
        batch_weight: usize,
        keep_going: usize,
    ) -> (Result<(), AlefError>, Told) {
        let (ready, answer) = oneshot::channel();
        let mut told = Vec::new();
        iterate(
            connection,
            sql,
            &Params::None,
            batch_rows,
            batch_weight,
            ready,
            |batch| {
                told.push(batch);
                told.len() < keep_going
            },
        );
        (answer.blocking_recv().unwrap(), told)
    }

    #[test]
    fn a_query_gives_up_to_its_limits_and_says_so() {
        let connection = memory();
        let all = |rows, weight| query(&connection, "SELECT s FROM t", &Params::None, rows, weight);
        assert_eq!(all(3, usize::MAX).unwrap().len(), 3);
        assert_eq!(
            all(2, usize::MAX).unwrap_err().code,
            ErrorCode::InvalidArgument
        );
        // A row of one text is 4: the name of its column, the text and its quotes.
        assert_eq!(all(10, 12).unwrap().len(), 3);
        assert_eq!(all(10, 11).unwrap_err().code, ErrorCode::InvalidArgument);
        assert_eq!(
            query(
                &connection,
                "INSERT INTO t VALUES (4, 'd')",
                &Params::None,
                10,
                100
            )
            .unwrap_err()
            .code,
            ErrorCode::InvalidArgument,
            "a statement that returns nothing is no query"
        );
    }

    #[test]
    fn exec_reports_the_rows_of_this_run_only() {
        let connection = memory();
        let done = exec(&connection, "UPDATE t SET n = n + 1", &Params::None).unwrap();
        assert_eq!(done.changes, 3);
        let done = exec(&connection, "CREATE TABLE u (x)", &Params::None).unwrap();
        assert_eq!(done.changes, 0, "not the 3 of the update before");
        let done = exec(
            &connection,
            "INSERT INTO u VALUES (1); INSERT INTO u VALUES (2)",
            &Params::None,
        )
        .unwrap();
        assert_eq!((done.changes, done.last_insert_id), (1, 2));
    }

    #[test]
    fn rows_are_handed_over_in_batches_by_count_and_by_weight() {
        let connection = memory();
        let (answer, told) = iterated(
            &connection,
            "SELECT n FROM t ORDER BY n",
            2,
            usize::MAX,
            usize::MAX,
        );
        assert!(answer.is_ok());
        let sizes: Vec<usize> = told
            .iter()
            .map(|batch| batch.as_ref().unwrap().len())
            .collect();
        assert_eq!(sizes, [2, 1]);
        let (_, told) = iterated(
            &connection,
            "SELECT n FROM t ORDER BY n",
            100,
            1,
            usize::MAX,
        );
        let sizes: Vec<usize> = told
            .iter()
            .map(|batch| batch.as_ref().unwrap().len())
            .collect();
        assert_eq!(sizes, [1, 1, 1], "a row heavier than a batch is a batch");
        let (answer, told) = iterated(
            &connection,
            "SELECT n FROM t WHERE n > 99",
            2,
            usize::MAX,
            usize::MAX,
        );
        assert!(
            answer.is_ok() && told.is_empty(),
            "no row: the call is answered, nothing is handed over"
        );
    }

    #[test]
    fn nobody_wanting_the_rows_ends_the_reading() {
        let connection = memory();
        let (answer, told) = iterated(&connection, "SELECT n FROM t ORDER BY n", 1, usize::MAX, 1);
        assert!(answer.is_ok());
        assert_eq!(
            told.len(),
            1,
            "after the first refusal nothing more was read"
        );
    }

    #[test]
    fn an_error_before_the_first_row_is_the_calls_and_one_after_it_is_the_streams() {
        let connection = memory();
        let (answer, told) = iterated(&connection, "SELEC n", 2, usize::MAX, usize::MAX);
        assert_eq!(answer.unwrap_err().code, ErrorCode::InvalidArgument);
        assert!(told.is_empty());
        let (answer, told) = iterated(
            &connection,
            "INSERT INTO t VALUES (9, 'z')",
            2,
            usize::MAX,
            usize::MAX,
        );
        assert_eq!(answer.unwrap_err().code, ErrorCode::InvalidArgument);
        assert!(told.is_empty());

        // The second row fails (abs of the smallest integer): the first reaches the reader, then the error.
        let sql =
            "SELECT CASE WHEN n = 2 THEN abs(-9223372036854775807 - 1) ELSE n END AS n FROM t";
        let (answer, told) = iterated(&connection, sql, 100, usize::MAX, usize::MAX);
        assert!(answer.is_ok(), "the first row was there");
        assert_eq!(told.len(), 2);
        assert_eq!(
            told[0].as_ref().unwrap().len(),
            1,
            "what was read before the error"
        );
        assert_eq!(
            told[1].as_ref().unwrap_err().code,
            ErrorCode::InvalidArgument
        );
    }

    #[test]
    fn what_goes_wrong_in_sqlite_is_told_in_the_words_of_alef() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("e.db");
        let first = open(&path, false, true).unwrap();
        first
            .execute_batch("CREATE TABLE t (a UNIQUE); INSERT INTO t VALUES (1);")
            .unwrap();

        let duplicate = first
            .execute("INSERT INTO t VALUES (1)", [])
            .map_err(sql_error)
            .unwrap_err();
        assert_eq!(duplicate.code, ErrorCode::InvalidArgument);
        assert_eq!(
            duplicate.details,
            Some(json!({ "sqlite": "SQLITE_CONSTRAINT_UNIQUE" }))
        );
        let syntax = first
            .prepare("SELEC")
            .map(drop)
            .map_err(sql_error)
            .unwrap_err();
        assert_eq!(syntax.code, ErrorCode::InvalidArgument);
        assert!(syntax.message.contains("syntax"), "{}", syntax.message);

        // Another connection that holds the database: asking for a read waits no longer than it is told to.
        let second = Connection::open(&path).unwrap();
        second.busy_timeout(Duration::from_millis(0)).unwrap();
        first.execute_batch("BEGIN EXCLUSIVE").unwrap();
        let busy = second
            .query_row("SELECT count(*) FROM t", [], |row| row.get::<_, i64>(0))
            .map_err(sql_error)
            .unwrap_err();
        assert_eq!(busy.code, ErrorCode::Busy);
        first.execute_batch("ROLLBACK").unwrap();

        let readonly = open(&path, true, false).unwrap();
        let refused = readonly
            .execute("INSERT INTO t VALUES (2)", [])
            .map_err(sql_error)
            .unwrap_err();
        assert_eq!(refused.code, ErrorCode::PermissionDenied);

        let text = folder.path().join("text.db");
        std::fs::write(
            &text,
            "not a database, only text that is long enough to be taken for a header",
        )
        .unwrap();
        assert_eq!(
            open(&text, false, true).unwrap_err().code,
            ErrorCode::InvalidArgument
        );
    }

    #[test]
    fn the_thread_of_a_connection_serves_one_job_at_a_time_and_ends_with_its_worker() {
        let worker = Worker::start(memory()).unwrap();
        let runtime = tokio::runtime::Builder::new_current_thread()
            .build()
            .unwrap();
        runtime.block_on(async {
            let first = worker.run(|connection| {
                std::thread::sleep(Duration::from_millis(50));
                connection
                    .execute_batch("INSERT INTO t VALUES (10, 'x')")
                    .map_err(sql_error)
            });
            let second = worker.run(|connection| {
                connection
                    .query_row("SELECT count(*) FROM t", [], |row| row.get::<_, i64>(0))
                    .map_err(sql_error)
            });
            let (one, two) = tokio::join!(first, second);
            one.unwrap();
            assert_eq!(two.unwrap(), 4, "the second job came after the first");
        });
        worker.shutdown();
        let refused = runtime.block_on(worker.run(|_| Ok(())));
        assert_eq!(refused.unwrap_err().code, ErrorCode::Closed);
        assert!(
            *worker.stopped().borrow(),
            "those who feed streams are told"
        );
    }
}
