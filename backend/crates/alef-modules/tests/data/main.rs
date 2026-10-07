// SPDX-License-Identifier: MIT OR Apache-2.0
//! The data modules (`fs`) through the registry.
#[path = "../common/mod.rs"]
mod common;

mod fs;
mod handles;
mod rights;
mod sqlite;
mod sqlite_files;
mod store;

use std::path::Path;

use alef_core::{
    registry::command::Reply,
    security::consent::{Consent, Decision, Right},
    AlefError,
};
use bytes::Bytes;
use serde_json::{json, Value};

use common::Fixture;

const MANIFEST: &str = include_str!("../fixtures/app.ktav");

/// A scope that covers `directory` and everything under it, written the way a manifest does.
pub fn scope_of(directory: &Path) -> String {
    format!("{}/**", directory.display().to_string().replace('\\', "/"))
}

/// An application whose manifest lists the given scopes, each with the decision of the user.
pub async fn fixture(read: &[(String, Decision)], write: &[(String, Decision)]) -> Fixture {
    let list = |scopes: &[(String, Decision)]| {
        let names: Vec<&str> = scopes.iter().map(|(scope, _)| scope.as_str()).collect();
        format!("[ {} ]", names.join(", "))
    };
    let text = MANIFEST.replace('\r', "").replace(
        "        read: []\n        write: []",
        &format!(
            "        read: {}\n        write: {}",
            list(read),
            list(write)
        ),
    );
    let mut consent = Consent::undecided();
    for (scope, decision) in read {
        consent.set(Right::scoped("fs.read", scope), *decision);
    }
    for (scope, decision) in write {
        consent.set(Right::scoped("fs.write", scope), *decision);
    }
    Fixture::new(Some(&text), &[]).await.with_consent(consent)
}

/// An application that may read and write everything under `directory`, as decided.
pub async fn fixture_in(directory: &Path, decision: Decision) -> Fixture {
    let scope = scope_of(directory);
    fixture(&[(scope.clone(), decision)], &[(scope, decision)]).await
}

pub fn at(path: &Path) -> Value {
    json!({ "path": path.to_string_lossy() })
}

pub async fn read(fixture: &Fixture, path: &Path) -> Result<Vec<u8>, AlefError> {
    match fixture.call_reply("fs.readFile", at(path), None).await? {
        Reply::Bytes(bytes) => Ok(bytes.to_vec()),
        other => panic!("expected bytes, got {other:?}"),
    }
}

pub async fn write(fixture: &Fixture, path: &Path, data: &[u8]) -> Result<(), AlefError> {
    fixture
        .call_reply("fs.writeFile", at(path), Some(Bytes::copy_from_slice(data)))
        .await
        .map(drop)
}
