// SPDX-License-Identifier: MIT OR Apache-2.0
//! The DNS label of the application origin (`https://<label>.alef`), derived by the host.
//!
//! Directory ids are encoded injectively (`[a-z0-9]` bytes pass through, every other byte
//! becomes `-xx`, so no encoding is a prefix of another) behind an `app-` class prefix; ids
//! whose encoding exceeds a DNS label fall back to an escape-safe prefix plus a collision-
//! resistant 96-bit SHA-256 truncation of the exact id; absolute injectivity is impossible
//! because a DNS label is capped at 63 bytes (pigeonhole). Packages use the first 28 hex
//! digits of their 64-digit
//! authority token behind a `pkg-` prefix (see `authority::Authority::package_token`). The
//! class prefixes are provably disjoint: directory labels start with `app-`, package labels
//! with `pkg-`, and the byte at index 1 differs (`p` vs `k`), so no label can belong to
//! both classes.

use crate::error::{AlefError, ErrorCode};
use crate::security::authority::Authority;
use sha2::{Digest, Sha256};

/// Suffix of every application origin host.
pub const ORIGIN_SUFFIX: &str = ".alef";
/// Class prefix of every directory-application label.
pub const DIRECTORY_PREFIX: &str = "app-";
/// Class prefix of every package-application label.
pub const PACKAGE_PREFIX: &str = "pkg-";
/// Longest label produced for an exact id before the hash fallback engages.
const MAX_PLAIN: usize = 63;
/// Prefix length kept of the encoding before appending the hash tail (4 + 34 + 1 + 24 = 63).
const PREFIX_LEN: usize = 34;
const HASH_LEN: usize = 24;

/// The DNS label of the application origin, without the `.alef` suffix.
///
/// `Authority::Directory` encodes the manifest id (validated: non-empty, ASCII, only
/// letters, digits, `.`, `-`, `_`, not starting with `.`); anything else is
/// `ManifestInvalid`. A package authority ignores the manifest id entirely: its label is
/// the first 28 hex digits of the authority token, which the launcher derives from the
/// verified archive, never from the package's own claims.
pub fn origin_host(id: &str, authority: &Authority) -> Result<String, AlefError> {
    if let Some(token) = authority.package_token(id) {
        // package_token always yields 64 hex digits, but a hand-built authority in tests
        // could carry anything; stay total instead of panicking.
        return token
            .get(..28)
            .map(|tail| format!("{PACKAGE_PREFIX}{tail}"))
            .ok_or_else(manifest_invalid);
    }
    plain_id(id)?;
    let encoded = encode(id);
    // The `app-` prefix also guarantees the label never starts with '-'.
    Ok(if DIRECTORY_PREFIX.len() + encoded.len() <= MAX_PLAIN {
        format!("{DIRECTORY_PREFIX}{encoded}")
    } else {
        format!(
            "{DIRECTORY_PREFIX}{}-{}",
            plain_prefix(&encoded),
            &hex(&Sha256::digest(id.as_bytes()))[..HASH_LEN]
        )
    })
}

/// The same charset the launcher's `plain_id` accepts for a manifest id.
fn plain_id(id: &str) -> Result<(), AlefError> {
    let plain = !id.is_empty()
        && !id.starts_with('.')
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'));
    if plain {
        Ok(())
    } else {
        Err(manifest_invalid())
    }
}

fn manifest_invalid() -> AlefError {
    AlefError::new(
        ErrorCode::ManifestInvalid,
        "id: must be a plain identifier (letters, digits, '.', '-', '_')",
    )
}

/// `[a-z0-9]` pass through, every other byte becomes `-xx` (lowercase hex); injective.
fn encode(id: &str) -> String {
    let mut out = String::with_capacity(id.len());
    for &byte in id.as_bytes() {
        match byte {
            b'a'..=b'z' | b'0'..=b'9' => out.push(byte as char),
            _ => out.push_str(&format!("-{byte:02x}")),
        }
    }
    out
}

/// Longest prefix of at most `PREFIX_LEN` bytes ending on a passthrough char or a
/// complete `-xx` escape, so the hash tail never glues onto a split escape.
fn plain_prefix(encoded: &str) -> String {
    let mut end = PREFIX_LEN.min(encoded.len());
    while !encoded.is_char_boundary(end) || splits_escape(&encoded[..end]) {
        end -= 1;
    }
    encoded[..end].to_owned()
}

/// True when the prefix ends inside an escape sequence (`-`, or `-` + one hex digit).
fn splits_escape(prefix: &str) -> bool {
    let bytes = prefix.as_bytes();
    match bytes {
        [.., b'-'] => true,
        [.., b'-', hex] if matches!(hex, b'a'..=b'f' | b'0'..=b'9') => true,
        _ => false,
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn host(id: &str) -> String {
        origin_host(id, &Authority::Directory).expect("host")
    }

    fn label_ids() -> Vec<String> {
        let mut ids: Vec<String> = [
            "org.example.app",
            "com.x.app",
            "a",
            "z9",
            "A",
            "a_b",
            "a-b",
            "_x",
            "-x",
            "0",
        ]
        .iter()
        .map(|s| s.to_string())
        .collect();
        for length in 1..=80 {
            for pattern in ["app.", "a-b_", "x"] {
                let id = pattern.repeat((length + pattern.len() - 1) / pattern.len());
                ids.push(id[..length.min(id.len())].to_string());
            }
        }
        ids
    }

    #[test]
    fn every_encoded_id_is_a_dns_safe_label_within_the_limit() {
        for id in label_ids() {
            let label = host(&id);
            assert!(!label.is_empty(), "{id}");
            assert!(label.len() <= 63, "{id}: {label}");
            assert!(
                label
                    .bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b == b'-'),
                "{id}: {label}"
            );
            // The class prefix guarantees a legal first char and never '-'.
            assert!(label.starts_with(DIRECTORY_PREFIX), "{id}: {label}");
            assert!(!label.ends_with('-'), "{id}: {label}");
        }
    }

    #[test]
    fn two_different_ids_never_map_to_one_host() {
        let mut ids = label_ids();
        for k in 0..64 {
            ids.push(format!("app{k}"));
        }
        for x in ["a", "b", "ab"] {
            for y in ["c", "d", "x"] {
                ids.push(format!("{x}.{y}"));
            }
        }
        ids.extend([
            "com.x.app".into(),
            "com-x-app".into(),
            "a_b".into(),
            "a-b".into(),
            "Ab".into(),
            "ab".into(),
            "a".into(),
            "b".into(),
        ]);
        // Ids deliberately shaped like package labels: the class prefixes must keep them
        // apart from `pkg-` labels anyway.
        ids.extend([
            "pkg".into(),
            "pkg0".into(),
            "pkg2dab420f56c1f3ba335f4d9e0a3c7b1".into(),
        ]);
        let ids: std::collections::BTreeSet<String> = ids.into_iter().collect();
        let mut seen = std::collections::HashMap::new();
        for id in &ids {
            let label = host(id);
            if let Some(other) = seen.insert(label.clone(), id) {
                panic!("collision: {other:?} and {id:?} both map to {label}");
            }
        }
        // Package authorities join the mix: every (authority, id) pair gets its own label.
        let fingerprints = [
            "0",
            "00",
            "aa",
            "0123456789abcdef0123456789ab",
            "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef",
        ];
        let mut pairs: Vec<(String, Authority)> = Vec::new();
        for id in &ids {
            pairs.push((id.clone(), Authority::Directory));
        }
        for fingerprint in fingerprints {
            let authority = Authority::SignedPackage {
                fingerprint: fingerprint.to_owned(),
            };
            for id in ["org.example.app", "org.example.other", "pkg", "0"] {
                pairs.push((id.to_owned(), authority.clone()));
            }
        }
        let mut seen = std::collections::HashMap::new();
        for (id, authority) in &pairs {
            let label = origin_host(id, authority).expect("host");
            if let Some((other_id, other_authority)) = seen.insert(label.clone(), (id, authority)) {
                panic!(
                    "collision: {:?} on {:?} and {:?} on {:?} both map to {label}",
                    other_id, other_authority, id, authority
                );
            }
            // Belt and braces beyond the index-1 prefix proof: no directory label may
            // equal any package label for any combination.
            if matches!(authority, Authority::Directory) {
                assert!(
                    !label.starts_with(PACKAGE_PREFIX),
                    "{id}: directory label {label} collides with the package class"
                );
            } else {
                assert!(
                    !label.starts_with(DIRECTORY_PREFIX),
                    "{id}: package label {label} collides with the directory class"
                );
            }
        }
    }

    #[test]
    fn every_id_with_a_fitting_encoding_round_trips_through_its_label() {
        // Inverse of `encode` for labels without the hash fallback.
        fn decode(label: &str) -> String {
            let mut id = String::new();
            let mut chars = label.chars();
            while let Some(c) = chars.next() {
                if c != '-' {
                    id.push(c);
                    continue;
                }
                let hex: String = chars.by_ref().take(2).collect();
                id.push(u8::from_str_radix(&hex, 16).expect("hex byte") as char);
            }
            id
        }
        for id in label_ids() {
            let encoded = encode(&id);
            if DIRECTORY_PREFIX.len() + encoded.len() > MAX_PLAIN {
                continue; // long ids are covered by the hash-form test below
            }
            let label = host(&id);
            assert_eq!(label, format!("{DIRECTORY_PREFIX}{encoded}"), "{id}");
            assert_eq!(decode(&label[DIRECTORY_PREFIX.len()..]), id, "{id}");
        }
    }

    #[test]
    fn ids_without_a_fitting_encoding_get_the_hash_form() {
        for id in label_ids() {
            let encoded = encode(&id);
            if DIRECTORY_PREFIX.len() + encoded.len() <= MAX_PLAIN {
                continue; // short ids are covered by the round-trip test above
            }
            let label = host(&id);
            assert!(label.len() <= 63, "{id}: {label}");
            assert_ne!(label, format!("{DIRECTORY_PREFIX}{encoded}"), "{id}");
            let bytes = label.as_bytes();
            assert!(bytes[bytes.len() - 25] == b'-', "{id}: {label}");
            assert!(
                bytes[bytes.len() - 24..]
                    .iter()
                    .all(|b| b.is_ascii_hexdigit() && !b.is_ascii_uppercase()),
                "{id}: {label}"
            );
        }
    }

    #[test]
    fn a_weird_id_is_rejected() {
        for id in ["", ".hidden", "a/b", "a b", "x:y", "héllo", "a\\b", ".."] {
            let error = origin_host(id, &Authority::Directory).expect_err(id);
            assert_eq!(error.code, ErrorCode::ManifestInvalid, "{id}");
        }
    }

    #[test]
    fn long_ids_use_a_stable_hash_tail_and_stay_deterministic() {
        let id = "app.".repeat(20);
        let first = host(&id);
        let second = host(&id);
        assert_eq!(first, second);
        assert!(first.len() <= 63, "{first}");
        assert!(first.starts_with(DIRECTORY_PREFIX), "{first}");
        // The hash tail (last 24 chars) must differ between two different long ids; the
        // escape-safe prefix before it may shrink, hence the tail-relative comparison.
        let other = host(&format!("{}x", "app.".repeat(20)));
        assert_ne!(&first[first.len() - 24..], &other[other.len() - 24..]);
    }

    #[test]
    fn package_authorities_use_the_authority_token_not_the_manifest_id() {
        let signed = |fingerprint: &str| Authority::SignedPackage {
            fingerprint: fingerprint.to_owned(),
        };
        // The authority token includes the app id: different ids with the same fingerprint
        // get DIFFERENT hosts.
        let one = origin_host("org.example.app", &signed("aa")).expect("host");
        let two = origin_host("org.example.other", &signed("aa")).expect("host");
        assert_ne!(one, two);
        let unsigned = origin_host(
            "org.example.app",
            &Authority::UnsignedPackage {
                archive_digest: "aa".into(),
            },
        )
        .expect("host");
        assert_ne!(one, unsigned);
        let token = signed("aa").package_token("org.example.app").unwrap();
        assert_eq!(one, format!("{PACKAGE_PREFIX}{}", &token[..28]));
        let directory = host("org.example.app");
        assert_ne!(directory, one);
    }

    #[test]
    fn known_vector() {
        assert_eq!(host("org.example.app"), "app-org-2eexample-2eapp");
        let authority = Authority::SignedPackage {
            fingerprint: "aa".into(),
        };
        let token = authority.package_token("org.example.app").unwrap();
        assert_eq!(
            origin_host("org.example.app", &authority).expect("host"),
            format!("{PACKAGE_PREFIX}{}", &token[..28])
        );
    }
}
