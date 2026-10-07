// SPDX-License-Identifier: MIT OR Apache-2.0
//! Databases in files (`sqlite`), through the registry.
use std::{path::Path, time::Duration};

use alef_core::{ids::StreamId, protocol::frame::Frame, security::consent::Decision, ErrorCode};
use serde_json::{json, Value};

use super::fixture_in;
use crate::common::Fixture;

mod files;

pub(crate) async fn open(app: &Fixture, path: &Path, extra: Value) -> u64 {
    let mut args = extra;
    args["path"] = json!(path.to_string_lossy());
    app.call("sqlite.open", args).await.unwrap()["db"]
        .as_u64()
        .unwrap()
}

pub(crate) async fn exec(app: &Fixture, db: u64, sql: &str, params: Value) -> Value {
    app.call(
        "sqlite.exec",
        json!({ "db": db, "sql": sql, "params": params }),
    )
    .await
    .unwrap()
}

pub(crate) async fn rows(app: &Fixture, db: u64, sql: &str, params: Value) -> Vec<Value> {
    let reply = app
        .call(
            "sqlite.query",
            json!({ "db": db, "sql": sql, "params": params }),
        )
        .await
        .unwrap();
    serde_json::from_value(reply).unwrap()
}

pub(crate) async fn denied(app: &Fixture, path: &Path, extra: Value) {
    let mut args = extra;
    args["path"] = json!(path.to_string_lossy());
    let error = app.call("sqlite.open", args).await.unwrap_err();
    assert_eq!(error.code, ErrorCode::PermissionDenied, "{path:?}");
}

pub(crate) async fn failure(app: &Fixture, command: &str, args: Value) -> alef_core::AlefError {
    app.call(command, args).await.unwrap_err()
}

/// The frames of a stream of rows, each acknowledged as a page does, until the stream ends.
async fn batches(app: &Fixture, id: StreamId) -> Vec<Vec<Value>> {
    let session = app.session();
    let mut reader = session.streams().reader(id).expect("an outgoing stream");
    let mut all = Vec::new();
    loop {
        match tokio::time::timeout(Duration::from_secs(20), reader.next_frame())
            .await
            .expect("the stream stalled")
        {
            Some(Frame::Json(batch)) => {
                session.streams().ack(id, batch.to_string().len()).unwrap();
                all.push(serde_json::from_value(batch).unwrap());
            }
            Some(Frame::End) | None => return all,
            other => panic!("unexpected {other:?}"),
        }
    }
}

fn stream_of(reply: &Value) -> StreamId {
    StreamId(reply["stream"].as_u64().unwrap())
}

#[tokio::test]
async fn values_go_in_and_come_out_as_what_they_are() {
    let directory = tempfile::tempdir().unwrap();
    let app = fixture_in(directory.path(), Decision::Allow).await;
    let db = open(&app, &directory.path().join("v.db"), json!({})).await;
    exec(
        &app,
        db,
        "CREATE TABLE t (i INTEGER, r REAL, s TEXT, b BLOB, n)",
        Value::Null,
    )
    .await;
    let done = exec(
        &app,
        db,
        "INSERT INTO t VALUES (?, ?, ?, ?, ?)",
        json!([42, 1.5, "ünï — 🌍", { "$blob": "AAH/" }, null]),
    )
    .await;
    assert_eq!(done, json!({ "changes": 1, "lastInsertId": 1 }));
    exec(
        &app,
        db,
        "INSERT INTO t (i, r) VALUES (?, ?)",
        json!([{ "$int": "9007199254740993" }, { "$real": "-Infinity" }]),
    )
    .await;
    exec(
        &app,
        db,
        "INSERT INTO t (i) VALUES (?)",
        json!([{ "$int": "-9223372036854775808" }]),
    )
    .await;
    exec(&app, db, "INSERT INTO t (i) VALUES (?)", json!([true])).await;
    exec(&app, db, "INSERT INTO t (r) VALUES (?)", json!([1e19])).await;

    let all = rows(
        &app,
        db,
        "SELECT i, r, s, b, n FROM t ORDER BY rowid",
        Value::Null,
    )
    .await;
    assert_eq!(all.len(), 5);
    assert_eq!(
        all[0],
        json!({ "i": 42, "r": 1.5, "s": "ünï — 🌍", "b": { "$blob": "AAH/" }, "n": null })
    );
    assert_eq!(all[1]["i"], json!({ "$int": "9007199254740993" }));
    assert_eq!(all[1]["r"], json!({ "$real": "-Infinity" }));
    assert_eq!(all[2]["i"], json!({ "$int": "-9223372036854775808" }));
    assert_eq!(all[3]["i"], json!(1), "a boolean is 1");
    assert_eq!(
        all[4]["r"],
        json!(1e19),
        "a number beyond 64 bits is a REAL"
    );

    let kinds = rows(
        &app,
        db,
        "SELECT typeof(i) AS i, typeof(r) AS r, typeof(s) AS s, typeof(b) AS b, typeof(n) AS n FROM t WHERE rowid = 1",
        Value::Null,
    )
    .await;
    assert_eq!(
        kinds[0],
        json!({ "i": "integer", "r": "real", "s": "text", "b": "blob", "n": "null" })
    );
    let computed = rows(
        &app,
        db,
        "SELECT 1 AS a, 2.5 AS b, 'x' AS c, NULL AS d, x'00' AS e",
        Value::Null,
    )
    .await;
    assert_eq!(
        computed[0],
        json!({ "a": 1, "b": 2.5, "c": "x", "d": null, "e": { "$blob": "AA==" } })
    );
}

#[tokio::test]
async fn parameters_are_a_list_or_names_and_a_wrong_one_is_told() {
    let directory = tempfile::tempdir().unwrap();
    let app = fixture_in(directory.path(), Decision::Allow).await;
    let db = open(&app, &directory.path().join("p.db"), json!({})).await;
    exec(&app, db, "CREATE TABLE t (a, b)", Value::Null).await;
    exec(
        &app,
        db,
        "INSERT INTO t VALUES (:a, @b)",
        json!({ "a": 1, "@b": 2 }),
    )
    .await;
    exec(
        &app,
        db,
        "INSERT INTO t VALUES ($a, :b)",
        json!({ "$a": 3, ":b": 4 }),
    )
    .await;
    exec(&app, db, "INSERT INTO t VALUES (?1, ?2)", json!([5, 6])).await;
    let all = rows(&app, db, "SELECT a, b FROM t ORDER BY a", Value::Null).await;
    assert_eq!(
        all,
        [
            json!({"a": 1, "b": 2}),
            json!({"a": 3, "b": 4}),
            json!({"a": 5, "b": 6})
        ]
    );
    let one = rows(&app, db, "SELECT b FROM t WHERE a = ?", json!([3])).await;
    assert_eq!(one, [json!({ "b": 4 })]);

    for (sql, params) in [
        ("INSERT INTO t VALUES (?, ?)", json!([1])),
        ("INSERT INTO t VALUES (?, ?)", json!([1, 2, 3])),
        ("INSERT INTO t VALUES (:a, :b)", json!({ "a": 1, "c": 2 })),
        ("INSERT INTO t VALUES (?, ?)", json!([[1], 2])),
        ("INSERT INTO t VALUES (?, ?)", json!("text")),
        ("INSERT INTO t VALUES (?, ?)", json!([{ "$int": "x" }, 1])),
    ] {
        let error = failure(
            &app,
            "sqlite.exec",
            json!({ "db": db, "sql": sql, "params": params }),
        )
        .await;
        assert_eq!(error.code, ErrorCode::InvalidArgument, "{sql} {params}");
    }
    assert_eq!(
        rows(&app, db, "SELECT count(*) AS n FROM t", Value::Null).await,
        [json!({ "n": 3 })],
        "nothing of the wrong ones was written"
    );
}

#[tokio::test]
async fn one_statement_with_parameters_and_several_without_and_each_for_what_it_is() {
    let directory = tempfile::tempdir().unwrap();
    let app = fixture_in(directory.path(), Decision::Allow).await;
    let db = open(&app, &directory.path().join("s.db"), json!({})).await;
    let batch = exec(
        &app,
        db,
        "CREATE TABLE a (x INTEGER PRIMARY KEY); INSERT INTO a VALUES (1); INSERT INTO a VALUES (2);",
        Value::Null,
    )
    .await;
    assert_eq!(batch, json!({ "changes": 1, "lastInsertId": 2 }));
    assert_eq!(
        exec(&app, db, "CREATE TABLE b (y)", Value::Null).await["changes"],
        json!(0),
        "a statement that changes no row says 0, not the number of the one before"
    );
    let updated = exec(&app, db, "UPDATE a SET x = x + 10", json!([])).await;
    assert_eq!(updated["changes"], json!(2));

    for (command, sql, params) in [
        (
            "sqlite.exec",
            "INSERT INTO a VALUES (?); INSERT INTO a VALUES (?)",
            json!([20, 21]),
        ),
        ("sqlite.exec", "SELECT x FROM a WHERE x > ?", json!([0])),
        ("sqlite.query", "INSERT INTO a VALUES (30)", Value::Null),
        ("sqlite.query", "CREATE TABLE c (z)", Value::Null),
    ] {
        let error = failure(
            &app,
            command,
            json!({ "db": db, "sql": sql, "params": params }),
        )
        .await;
        assert_eq!(error.code, ErrorCode::InvalidArgument, "{command} {sql}");
    }
}

#[tokio::test]
async fn an_sql_error_is_the_callers_own_and_says_what_failed() {
    let directory = tempfile::tempdir().unwrap();
    let app = fixture_in(directory.path(), Decision::Allow).await;
    let db = open(&app, &directory.path().join("e.db"), json!({})).await;
    exec(
        &app,
        db,
        "PRAGMA foreign_keys = ON; CREATE TABLE p (id INTEGER PRIMARY KEY, u TEXT UNIQUE, n TEXT NOT NULL, c INTEGER CHECK (c > 0)); CREATE TABLE q (p_id REFERENCES p(id)); INSERT INTO p VALUES (1, 'one', 'n', 1);",
        Value::Null,
    )
    .await;
    let sqlite_of =
        |error: &alef_core::AlefError| error.details.as_ref().map(|d| d["sqlite"].clone());
    for (sql, extended) in [
        (
            "INSERT INTO p VALUES (2, 'one', 'n', 1)",
            json!("SQLITE_CONSTRAINT_UNIQUE"),
        ),
        (
            "INSERT INTO p VALUES (1, 'two', 'n', 1)",
            json!("SQLITE_CONSTRAINT_PRIMARYKEY"),
        ),
        (
            "INSERT INTO p VALUES (3, 'three', NULL, 1)",
            json!("SQLITE_CONSTRAINT_NOTNULL"),
        ),
        (
            "INSERT INTO p VALUES (4, 'four', 'n', 0)",
            json!("SQLITE_CONSTRAINT_CHECK"),
        ),
        (
            "INSERT INTO q VALUES (99)",
            json!("SQLITE_CONSTRAINT_FOREIGNKEY"),
        ),
    ] {
        let error = failure(&app, "sqlite.exec", json!({ "db": db, "sql": sql })).await;
        assert_eq!(error.code, ErrorCode::InvalidArgument, "{sql}");
        assert_eq!(sqlite_of(&error), Some(extended), "{sql}");
        assert!(error.message.contains("constraint"), "{}", error.message);
    }
    let syntax = failure(&app, "sqlite.query", json!({ "db": db, "sql": "SELEC 1" })).await;
    assert_eq!(syntax.code, ErrorCode::InvalidArgument);
    assert!(syntax.message.contains("syntax"), "{}", syntax.message);
    let table = failure(
        &app,
        "sqlite.query",
        json!({ "db": db, "sql": "SELECT * FROM nothing" }),
    )
    .await;
    assert_eq!(table.code, ErrorCode::InvalidArgument);
    assert!(table.message.contains("nothing"), "{}", table.message);
}

#[tokio::test]
async fn a_prepared_statement_runs_again_and_ends_when_asked_or_with_its_database() {
    let directory = tempfile::tempdir().unwrap();
    let app = fixture_in(directory.path(), Decision::Allow).await;
    let db = open(&app, &directory.path().join("t.db"), json!({})).await;
    exec(&app, db, "CREATE TABLE t (a INTEGER, b TEXT)", Value::Null).await;
    let insert = app
        .call(
            "sqlite.prepare",
            json!({ "db": db, "sql": "INSERT INTO t VALUES (?, ?)" }),
        )
        .await
        .unwrap()["statement"]
        .as_u64()
        .unwrap();
    for (a, b) in [(1, "x"), (2, "y"), (3, "z")] {
        let done = app
            .call(
                "sqlite.exec",
                json!({ "statement": insert, "params": [a, b] }),
            )
            .await
            .unwrap();
        assert_eq!(done["changes"], json!(1));
    }
    let select = app
        .call(
            "sqlite.prepare",
            json!({ "db": db, "sql": "SELECT b FROM t WHERE a >= ? ORDER BY a" }),
        )
        .await
        .unwrap()["statement"]
        .as_u64()
        .unwrap();
    let from = |a: i64| json!({ "statement": select, "params": [a] });
    assert_eq!(
        app.call("sqlite.query", from(2)).await.unwrap(),
        json!([{ "b": "y" }, { "b": "z" }])
    );
    assert_eq!(
        app.call("sqlite.query", from(3)).await.unwrap(),
        json!([{ "b": "z" }])
    );
    let streamed = app.call("sqlite.iterate", from(1)).await.unwrap();
    let all: Vec<Value> = batches(&app, stream_of(&streamed)).await.concat();
    assert_eq!(
        all,
        [json!({"b": "x"}), json!({"b": "y"}), json!({"b": "z"})]
    );

    for bad in [
        json!({ "db": db, "sql": "SELEC", "statement": 1 }),
        json!({ "db": db }),
        json!({ "sql": "SELECT 1" }),
        json!({}),
    ] {
        let error = failure(&app, "sqlite.query", bad.clone()).await;
        assert_eq!(error.code, ErrorCode::InvalidArgument, "{bad}");
    }
    let error = failure(
        &app,
        "sqlite.prepare",
        json!({ "db": db, "sql": "SELEC 1" }),
    )
    .await;
    assert_eq!(
        error.code,
        ErrorCode::InvalidArgument,
        "a bad statement is told when prepared"
    );
    assert_eq!(
        app.session().resources().len(),
        3,
        "the database and two statements"
    );

    app.call("sqlite.finalize", json!({ "statement": insert }))
        .await
        .unwrap();
    let error = failure(
        &app,
        "sqlite.exec",
        json!({ "statement": insert, "params": [9, "q"] }),
    )
    .await;
    assert_eq!(
        error.code,
        ErrorCode::NotFound,
        "a finalized statement is gone"
    );
    app.call("sqlite.close", json!({ "db": db })).await.unwrap();
    let error = failure(&app, "sqlite.query", from(1)).await;
    assert_eq!(
        error.code,
        ErrorCode::NotFound,
        "a statement is nothing without its database"
    );
}

#[tokio::test]
async fn rows_come_in_batches_in_order_and_what_cuts_the_stream_frees_the_connection() {
    let directory = tempfile::tempdir().unwrap();
    let app = fixture_in(directory.path(), Decision::Allow).await;
    let db = open(&app, &directory.path().join("big.db"), json!({})).await;
    exec(&app, db, "CREATE TABLE t (n INTEGER)", Value::Null).await;
    exec(
        &app,
        db,
        "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM c WHERE x < 5000) INSERT INTO t SELECT x FROM c",
        Value::Null,
    )
    .await;

    let started = app
        .call(
            "sqlite.iterate",
            json!({ "db": db, "sql": "SELECT n FROM t ORDER BY n" }),
        )
        .await
        .unwrap();
    let frames = batches(&app, stream_of(&started)).await;
    assert!(frames.len() >= 10, "{} frames", frames.len());
    assert!(frames
        .iter()
        .all(|batch| !batch.is_empty() && batch.len() <= 500));
    let numbers: Vec<i64> = frames
        .concat()
        .iter()
        .map(|row| row["n"].as_i64().unwrap())
        .collect();
    assert_eq!(
        numbers,
        (1..=5000).collect::<Vec<i64>>(),
        "every row, in order, once"
    );

    let none = app
        .call(
            "sqlite.iterate",
            json!({ "db": db, "sql": "SELECT n FROM t WHERE n < 0" }),
        )
        .await
        .unwrap();
    assert!(
        batches(&app, stream_of(&none)).await.is_empty(),
        "no row: the stream just ends"
    );

    let cut = app
        .call(
            "sqlite.iterate",
            json!({ "db": db, "sql": "SELECT n FROM t ORDER BY n" }),
        )
        .await
        .unwrap();
    let id = stream_of(&cut);
    let session = app.session();
    let mut reader = session.streams().reader(id).unwrap();
    assert!(matches!(reader.next_frame().await, Some(Frame::Json(_))));
    session.streams().close(id).unwrap();
    let count = tokio::time::timeout(
        Duration::from_secs(20),
        app.call(
            "sqlite.query",
            json!({ "db": db, "sql": "SELECT count(*) AS n FROM t" }),
        ),
    )
    .await
    .expect("the connection stayed with the iteration")
    .unwrap();
    assert_eq!(count, json!([{ "n": 5000 }]));

    for sql in ["SELEC", "INSERT INTO t VALUES (1)", "SELECT * FROM nothing"] {
        let error = failure(&app, "sqlite.iterate", json!({ "db": db, "sql": sql })).await;
        assert_eq!(
            error.code,
            ErrorCode::InvalidArgument,
            "{sql}: told by the call, not by the stream"
        );
    }
}

#[tokio::test]
async fn a_transaction_is_begin_and_commit_or_rollback() {
    let directory = tempfile::tempdir().unwrap();
    let app = fixture_in(directory.path(), Decision::Allow).await;
    let path = directory.path().join("x.db");
    let db = open(&app, &path, json!({})).await;
    exec(&app, db, "CREATE TABLE t (a)", Value::Null).await;
    let count = |db: u64| {
        let app = &app;
        async move {
            rows(app, db, "SELECT count(*) AS n FROM t", Value::Null).await[0]["n"]
                .as_i64()
                .unwrap()
        }
    };
    exec(&app, db, "BEGIN", Value::Null).await;
    exec(&app, db, "INSERT INTO t VALUES (1)", Value::Null).await;
    assert_eq!(count(db).await, 1, "inside, the row is there");
    exec(&app, db, "ROLLBACK", Value::Null).await;
    assert_eq!(count(db).await, 0, "rolled back");
    exec(&app, db, "BEGIN", Value::Null).await;
    exec(&app, db, "INSERT INTO t VALUES (2)", Value::Null).await;
    exec(&app, db, "COMMIT", Value::Null).await;
    app.call("sqlite.close", json!({ "db": db })).await.unwrap();
    let again = open(&app, &path, json!({ "readonly": true })).await;
    assert_eq!(count(again).await, 1, "committed and kept in the file");
}

#[tokio::test]
async fn a_database_closes_even_when_nobody_reads_the_rows_it_is_sending() {
    let directory = tempfile::tempdir().unwrap();
    let app = fixture_in(directory.path(), Decision::Allow).await;
    let db = open(&app, &directory.path().join("held.db"), json!({})).await;
    exec(&app, db, "CREATE TABLE t (n INTEGER)", Value::Null).await;
    exec(
        &app,
        db,
        "WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM c WHERE x < 5000) INSERT INTO t SELECT x FROM c",
        Value::Null,
    )
    .await;
    // 5000 rows of 4 KiB: far more than the window of a stream, and nobody reads them.
    let started = app
        .call(
            "sqlite.iterate",
            json!({ "db": db, "sql": "SELECT n, hex(randomblob(2048)) AS pad FROM t" }),
        )
        .await
        .unwrap();
    let _unread = stream_of(&started);
    tokio::time::timeout(
        Duration::from_secs(20),
        app.call("sqlite.close", json!({ "db": db })),
    )
    .await
    .expect("the database did not close: its thread waits for a reader")
    .unwrap();
}
