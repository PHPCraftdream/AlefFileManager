// SPDX-License-Identifier: MIT OR Apache-2.0
//! The `cli` module through the registry, against the `node` on the PATH: what a run gives back,
//! the rights each command needs, and that every child and grandchild is gone when it must be.
#![allow(dead_code)] // each test file uses a different part
pub mod edges;
pub mod exec;
pub mod spawn;

#[path = "../../net/shared/pipe.rs"]
pub mod pipe;

use std::{path::Path, time::Duration};

use alef_core::{AlefError, ErrorCode};

use crate::common::Fixture;

const BASE: &str = include_str!("../../fixtures/app.ktav");

/// An application that may run the given programs.
pub fn manifest(exec: &[&str]) -> String {
    let text = BASE.replace('\r', "").replace(
        "        exec: []",
        &format!("        exec: [ {} ]", exec.join(", ")),
    );
    assert!(text.contains("exec: [ ") || exec.is_empty());
    text
}

/// The same, and one scope of `fs.read` too.
pub fn manifest_with_read(exec: &[&str], read: &str) -> String {
    manifest(exec).replace("        read: []", &format!("        read: [ {read} ]"))
}

fn code<T>(result: Result<T, AlefError>) -> ErrorCode {
    match result {
        Ok(_) => panic!("an error was expected"),
        Err(error) => error.code,
    }
}

/// Waits until the predicate holds, and fails with `what` when the time runs out.
async fn until<F, Fut>(mut predicate: F, ms: u64, what: &str)
where
    F: FnMut() -> Fut,
    Fut: std::future::Future<Output = bool>,
{
    let deadline = tokio::time::Instant::now() + Duration::from_millis(ms);
    while !predicate().await {
        assert!(
            tokio::time::Instant::now() < deadline,
            "never {what} in time"
        );
        tokio::time::sleep(Duration::from_millis(25)).await;
    }
}

/// Whether a process of the given id still answers signal 0, asked through a run of `node`.
async fn alive(app: &Fixture, pid: u64) -> bool {
    let line = format!(
        r#"node -e "try{{process.kill({pid},0);if(process.platform==='linux'){{const s=require('fs').readFileSync('/proc/{pid}/stat','utf8');if(s.slice(s.lastIndexOf(')')+2).startsWith('Z'))process.exit(1)}}}}catch(e){{if(e.code==='ESRCH'||e.code==='ENOENT')process.exit(1);throw e}}""#
    );
    let reply = app
        .call("cli.exec", serde_json::json!({ "commandLine": line }))
        .await
        .expect("the probe itself runs");
    reply["code"].as_i64() == Some(0)
}

/// The pid a child wrote into its file, once it wrote one.
fn pid_of(file: &Path) -> Option<u64> {
    std::fs::read_to_string(file).ok()?.trim().parse().ok()
}
