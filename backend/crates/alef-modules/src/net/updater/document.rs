// SPDX-License-Identifier: MIT OR Apache-2.0
//! Strict manifest-feed document parsing.
use alef_core::{security::manifest::compare_versions, AlefError, ErrorCode};
use serde::Deserialize;
use url::Url;

pub(super) const MAX_DOCUMENT: usize = 64 * 1024;

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Document {
    pub version: String,
    #[serde(default, deserialize_with = "non_null_string")]
    pub notes: Option<String>,
    pub package: String,
    pub sha256: String,
}

fn non_null_string<'de, D: serde::Deserializer<'de>>(d: D) -> Result<Option<String>, D::Error> {
    String::deserialize(d).map(Some)
}

fn bad() -> AlefError {
    AlefError::new(ErrorCode::UpdateBadDocument, "invalid updater document")
}

pub(super) fn parse(
    data: &[u8],
    endpoint: &Url,
    current: &str,
) -> Result<Option<Document>, AlefError> {
    if data.len() > MAX_DOCUMENT {
        return Err(AlefError::new(
            ErrorCode::UpdateTooLarge,
            "updater document exceeds 64 KiB",
        ));
    }
    let doc: Document = serde_json::from_slice(data).map_err(|_| bad())?;
    let ordering = compare_versions(&doc.version, current).map_err(|_| bad())?;
    if doc.package.is_empty() || doc.package.chars().any(char::is_control) {
        return Err(bad());
    }
    if doc.sha256.len() != 64
        || !doc
            .sha256
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(bad());
    }
    let package = endpoint.join(&doc.package).map_err(|_| bad())?;
    if package.username() != ""
        || package.password().is_some()
        || package.fragment().is_some()
        || !same_origin(endpoint, &package)
        || !matches!(package.scheme(), "https" | "http")
    {
        return Err(bad());
    }
    if ordering != std::cmp::Ordering::Greater {
        return Ok(None);
    }
    Ok(Some(Document {
        package: package.to_string(),
        ..doc
    }))
}

fn same_origin(a: &Url, b: &Url) -> bool {
    a.scheme() == b.scheme()
        && a.host_str() == b.host_str()
        && a.port_or_known_default() == b.port_or_known_default()
}

#[cfg(test)]
mod tests {
    use super::*;
    fn endpoint() -> Url {
        Url::parse("https://updates.example/feed/latest.json").unwrap()
    }
    fn doc(version: &str, package: &str, notes: &str, hash: &str) -> Vec<u8> {
        format!(
            r#"{{"version":"{version}","package":"{package}","notes":{notes},"sha256":"{hash}"}}"#
        )
        .into_bytes()
    }
    const HASH: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
    #[test]
    fn accepts_relative_same_origin_urls() {
        assert!(parse(
            &doc("1.1.0", "../../../pkg", "\"ok\"", HASH),
            &endpoint(),
            "1.0.0"
        )
        .unwrap()
        .is_some());
    }
    #[test]
    fn versions_return_none_when_not_newer() {
        for v in ["1.0.0", "0.9.0", "1.0.0+build.1"] {
            assert!(
                parse(&doc(v, "pkg", "\"notes\"", HASH), &endpoint(), "1.0.0")
                    .unwrap()
                    .is_none()
            );
        }
    }
    #[test]
    fn higher_prereleases_compare_correctly() {
        for (v, current) in [("1.1.0-alpha.2", "1.1.0-alpha.1"), ("1.1.0", "1.1.0-rc.1")] {
            assert!(
                parse(&doc(v, "pkg", "\"notes\"", HASH), &endpoint(), current)
                    .unwrap()
                    .is_some()
            );
        }
    }
    #[test]
    fn rejects_bad_versions_with_exact_code() {
        for v in ["bad", "1.0"] {
            assert_eq!(
                parse(&doc(v, "pkg", "\"notes\"", HASH), &endpoint(), "1.0.0")
                    .unwrap_err()
                    .code,
                ErrorCode::UpdateBadDocument
            );
        }
    }
    #[test]
    fn accepts_multiline_notes() {
        assert!(parse(
            &doc("1.1.0", "pkg", "\"line one\\nline two\"", HASH),
            &endpoint(),
            "1.0.0"
        )
        .unwrap()
        .is_some());
    }
    #[test]
    fn validates_malformed_old_docs_and_null_notes() {
        for bytes in [
            doc("1.0.0", "pkg", "null", HASH),
            doc("1.0.0", "", "\"notes\"", HASH),
        ] {
            assert_eq!(
                parse(&bytes, &endpoint(), "1.0.0").unwrap_err().code,
                ErrorCode::UpdateBadDocument
            );
        }
    }
    #[test]
    fn rejects_bad_urls() {
        for p in [
            "https://user@updates.example/pkg",
            "https://updates.example/pkg#frag",
            "http://updates.example/pkg",
            "https://updates.example:444/pkg",
            "https://evil.example/pkg",
        ] {
            assert_eq!(
                parse(&doc("2.0.0", p, "\"notes\"", HASH), &endpoint(), "1.0.0")
                    .unwrap_err()
                    .code,
                ErrorCode::UpdateBadDocument
            );
        }
    }
    #[test]
    fn rejects_unknown_fields_and_bad_hashes() {
        assert_eq!(parse(br#"{"version":"2.0.0","package":"pkg","notes":"notes","sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa","extra":1}"#, &endpoint(), "1.0.0").unwrap_err().code, ErrorCode::UpdateBadDocument);
        assert_eq!(
            parse(
                &doc("2.0.0", "pkg", "\"notes\"", "ABC"),
                &endpoint(),
                "1.0.0"
            )
            .unwrap_err()
            .code,
            ErrorCode::UpdateBadDocument
        );
    }
    #[test]
    fn rejects_oversized_document() {
        assert_eq!(
            parse(&vec![b' '; MAX_DOCUMENT + 1], &endpoint(), "1.0.0")
                .unwrap_err()
                .code,
            ErrorCode::UpdateTooLarge
        );
    }
}
