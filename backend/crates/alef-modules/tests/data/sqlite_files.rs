// SPDX-License-Identifier: MIT OR Apache-2.0
//! Where a database may be, what it may reach and when it ends (`sqlite`), through the registry.
use alef_core::{security::consent::Decision, ErrorCode};
use serde_json::{json, Value};

use super::{fixture, fixture_in, scope_of};
use crate::sqlite::{denied, exec, failure, open, rows};

#[tokio::test]
async fn a_database_is_opened_as_the_rights_allow_and_a_file_is_what_it_is() {
    let readable = tempfile::tempdir().unwrap();
    let writable = tempfile::tempdir().unwrap();
    let outside = tempfile::tempdir().unwrap();
    let existing = readable.path().join("r.db");
    {
        let connection = rusqlite::Connection::open(&existing).unwrap();
        connection
            .execute_batch("CREATE TABLE t (a); INSERT INTO t VALUES (7);")
            .unwrap();
    }
    let app = fixture(
        &[(scope_of(readable.path()), Decision::Allow)],
        &[(scope_of(writable.path()), Decision::Allow)],
    )
    .await;
    let db = open(&app, &existing, json!({ "readonly": true })).await;
    assert_eq!(
        rows(&app, db, "SELECT a FROM t", Value::Null).await,
        [json!({ "a": 7 })]
    );
    let error = failure(
        &app,
        "sqlite.exec",
        json!({ "db": db, "sql": "INSERT INTO t VALUES (8)" }),
    )
    .await;
    assert_eq!(
        error.code,
        ErrorCode::PermissionDenied,
        "opened to read only"
    );
    denied(&app, &existing, json!({})).await; // to write, it needs the right to write
    denied(&app, &writable.path().join("w.db"), json!({})).await; // to write, it needs the right to read as well
    denied(
        &app,
        &writable.path().join("w.db"),
        json!({ "readonly": true }),
    )
    .await;
    denied(&app, &outside.path().join("o.db"), json!({})).await;
    denied(
        &app,
        &outside.path().join("o.db"),
        json!({ "readonly": true }),
    )
    .await;
    assert!(!outside.path().join("o.db").exists());
}

#[tokio::test]
async fn a_file_that_is_not_there_or_not_a_database_is_told_as_such() {
    let directory = tempfile::tempdir().unwrap();
    let app = fixture_in(directory.path(), Decision::Allow).await;
    let missing = directory.path().join("missing.db");
    for extra in [json!({ "readonly": true }), json!({ "create": false })] {
        let mut args = extra.clone();
        args["path"] = json!(missing.to_string_lossy());
        let error = app.call("sqlite.open", args).await.unwrap_err();
        assert_eq!(error.code, ErrorCode::NotFound, "{extra}");
    }
    assert!(!missing.exists(), "nothing was made");
    let deep = directory.path().join("no").join("folder").join("x.db");
    let error = app
        .call("sqlite.open", json!({ "path": deep.to_string_lossy() }))
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::NotFound);
    let error = app
        .call(
            "sqlite.open",
            json!({ "path": directory.path().to_string_lossy() }),
        )
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::IsADirectory);
    let text = directory.path().join("text.db");
    std::fs::write(
        &text,
        "this is not a database, only text that is long enough to be read as a header",
    )
    .unwrap();
    let error = app
        .call("sqlite.open", json!({ "path": text.to_string_lossy() }))
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidArgument);
    assert_eq!(app.session().resources().len(), 0, "nothing stayed open");
    let made = open(&app, &missing, json!({})).await;
    exec(&app, made, "CREATE TABLE t (a)", Value::Null).await;
    assert!(missing.exists(), "create is the default");
}

#[tokio::test]
async fn one_file_is_not_real_for_reading_and_a_stand_in_for_writing_and_a_stand_in_keeps_the_database(
) {
    let both = tempfile::tempdir().unwrap();
    let file = both.path().join("d.db");
    let scope = scope_of(both.path());
    let mixed = fixture(
        &[(scope.clone(), Decision::Allow)],
        &[(scope.clone(), Decision::Substitute)],
    )
    .await;
    let error = mixed
        .call("sqlite.open", json!({ "path": file.to_string_lossy() }))
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::PermissionDenied);

    let shadowed = fixture(
        &[(scope.clone(), Decision::Substitute)],
        &[(scope, Decision::Substitute)],
    )
    .await;
    let db = open(&shadowed, &file, json!({})).await;
    exec(
        &shadowed,
        db,
        "CREATE TABLE t (a); INSERT INTO t VALUES (1);",
        Value::Null,
    )
    .await;
    assert_eq!(
        rows(&shadowed, db, "SELECT a FROM t", Value::Null).await,
        [json!({ "a": 1 })]
    );
    assert!(
        !file.exists(),
        "the database is in the stand-in, not in the folder"
    );
    assert_eq!(std::fs::read_dir(both.path()).unwrap().count(), 0);
}

#[tokio::test]
async fn sql_cannot_reach_another_file_or_load_code() {
    let directory = tempfile::tempdir().unwrap();
    let elsewhere = tempfile::tempdir().unwrap();
    let app = fixture_in(directory.path(), Decision::Allow).await;
    let db = open(&app, &directory.path().join("g.db"), json!({})).await;
    exec(
        &app,
        db,
        "CREATE TABLE t (a); INSERT INTO t VALUES (1);",
        Value::Null,
    )
    .await;

    let target = elsewhere.path().join("other.db");
    let sql_target = target.to_string_lossy().replace('\'', "''");
    for sql in [
        format!("ATTACH DATABASE '{sql_target}' AS other"),
        format!("VACUUM INTO '{sql_target}'"),
        "SELECT load_extension('nothing')".to_owned(),
    ] {
        let error = failure(&app, "sqlite.exec", json!({ "db": db, "sql": sql })).await;
        assert_ne!(
            error.code,
            ErrorCode::Internal,
            "{sql}: refused, not failed"
        );
        assert!(
            !target.exists(),
            "{sql}: the file outside the scope was made"
        );
    }
    assert_eq!(std::fs::read_dir(elsewhere.path()).unwrap().count(), 0);
    exec(&app, db, "VACUUM", Value::Null).await;
    exec(&app, db, "PRAGMA user_version = 3", Value::Null).await;
    assert_eq!(
        rows(&app, db, "PRAGMA user_version", Value::Null).await,
        [json!({ "user_version": 3 })]
    );
}

#[tokio::test]
async fn a_database_belongs_to_its_document_and_closes_with_it() {
    let directory = tempfile::tempdir().unwrap();
    let mut app = fixture_in(directory.path(), Decision::Allow).await;
    let path = directory.path().join("own.db");
    let db = open(&app, &path, json!({})).await;
    exec(&app, db, "CREATE TABLE t (a)", Value::Null).await;
    let other = app.open_window(2).await;
    let error = app
        .call_as(
            &other,
            "sqlite.query",
            json!({ "db": db, "sql": "SELECT 1" }),
        )
        .await
        .unwrap_err();
    assert_eq!(
        error.code,
        ErrorCode::NotFound,
        "another document has no such database"
    );
    app.call("sqlite.close", json!({ "db": db })).await.unwrap();
    let error = failure(&app, "sqlite.query", json!({ "db": db, "sql": "SELECT 1" })).await;
    assert_eq!(error.code, ErrorCode::NotFound);
    std::fs::remove_file(&path).unwrap(); // a closed database holds nothing (Windows would refuse)

    let again = open(&app, &path, json!({})).await;
    exec(&app, again, "CREATE TABLE t (a)", Value::Null).await;
    assert_eq!(app.session().resources().len(), 1);
    app.reload_document().await;
    assert_eq!(
        app.session().resources().len(),
        0,
        "the new document has none"
    );
    std::fs::remove_file(&path).unwrap(); // closed with the old document, thread and connection too
}
