// SPDX-License-Identifier: MIT OR Apache-2.0
//! The manifest of the tests of the network: an application with the given scopes.
use crate::common::Fixture;

const MANIFEST: &str = include_str!("../../fixtures/app.ktav");

/// The scopes of `net.http` and of `net.socket` the application lists.
pub fn manifest(http: &[String], sockets: &[&str]) -> String {
    let list = |items: Vec<&str>| format!("[ {} ]", items.join(", "));
    let text = MANIFEST
        .replace('\r', "")
        .replace(
            "        http: []",
            &format!(
                "        http: {}",
                list(http.iter().map(String::as_str).collect())
            ),
        )
        .replace(
            "        socket: []",
            &format!("        socket: {}", list(sockets.to_vec())),
        );
    assert!(text.contains("http: [ ") || http.is_empty());
    text
}

pub async fn app(http: &[String], sockets: &[&str]) -> Fixture {
    Fixture::new(Some(&manifest(http, sockets)), &[]).await
}
