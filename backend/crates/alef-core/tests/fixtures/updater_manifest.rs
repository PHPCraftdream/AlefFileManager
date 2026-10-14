// SPDX-License-Identifier: MIT OR Apache-2.0
use alef_core::security::manifest::{compare_versions, Manifest};
use alef_core::ErrorCode;

const MINIMAL: &str = include_str!("minimal.ktav");

#[test]
fn version_comparison_has_strict_semver_precedence_and_ignores_build_metadata() {
    use std::cmp::Ordering::{Equal, Greater, Less};
    for (left, right, expected) in [
        ("1.0.0", "1.0.0", Equal),
        ("1.0.1", "1.0.0", Greater),
        ("1.0.0", "1.0.1", Less),
        ("2.0.0", "1.99.99", Greater),
        ("0.0.1", "0.0.0", Greater),
        ("10.0.0", "9.99.99", Greater),
        ("1.0.0-alpha", "1.0.0", Less),
        ("1.0.0-alpha.1", "1.0.0-alpha.beta", Less),
        ("1.0.0-beta.2", "1.0.0-beta.11", Less),
        ("1.0.0-rc.1", "1.0.0", Less),
        ("1.0.0+one", "1.0.0+two", Equal),
        ("1.0.0-alpha+one", "1.0.0-alpha+two", Equal),
    ] {
        assert_eq!(
            compare_versions(left, right).unwrap(),
            expected,
            "{left} vs {right}"
        );
    }
    for (left, right) in [
        ("v1.0.0", "1.0.0"),
        ("1.0", "1.0.0"),
        (" 1.0.0", "1.0.0"),
        ("1.0.0", "1.0.0 "),
        ("01.0.0", "1.0.0"),
        ("1.0.0-01", "1.0.0"),
    ] {
        assert!(
            compare_versions(left, right).is_err(),
            "{left:?} vs {right:?}"
        );
    }
}

#[test]
fn updater_is_optional_non_null_closed_and_validates_key_endpoint_and_scope() {
    let base = Manifest::from_ktav_str(MINIMAL).unwrap();
    assert!(base.updater.is_none());
    let valid = MINIMAL.replace("        http: []", "        http: [\n            :: https://updates.example/*\n        ]")
        + "\nupdater: { endpoint: https://updates.example/feed, publicKey: AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA= }\n";
    let parsed = Manifest::from_ktav_str(&valid).unwrap();
    assert_eq!(
        parsed.updater.as_ref().unwrap().endpoint,
        "https://updates.example/feed"
    );
    let json = serde_json::to_value(&parsed).unwrap();
    assert_eq!(serde_json::from_value::<Manifest>(json).unwrap(), parsed);
    for suffix in ["\nupdater: null\n", "\nupdater: { endpoint: https://updates.example/feed, publicKey: AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=, unknown: true }\n"] {
        let error = Manifest::from_ktav_str(&(MINIMAL.to_owned() + suffix)).unwrap_err();
        assert_eq!(error.code, ErrorCode::ManifestInvalid);
    }
    for (endpoint, key, scope) in [
        (
            "http://127.0.0.1:8080/feed?channel=stable",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
            "http://127.0.0.1:8080/*",
        ),
        (
            "http://[::1]:8080/feed",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
            "http://[::1]:8080/*",
        ),
        (
            "https://updates.example/feed?channel=stable",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
            "https://updates.example/*",
        ),
    ] {
        let source =
            MINIMAL.replace(
                "        http: []",
                &format!("        http: [\n            :: {scope}\n        ]"),
            ) + &format!("\nupdater: {{\n    endpoint:: {endpoint}\n    publicKey:: {key}\n}}\n");
        let parsed = Manifest::from_ktav_str(&source)
            .unwrap_or_else(|error| panic!("{endpoint}: {error:?}"));
        assert_eq!(parsed.updater.as_ref().unwrap().endpoint, endpoint);
    }
    for (endpoint, key, scope) in [
        (
            "http://updates.example/feed",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
            "https://updates.example/*",
        ),
        (
            "https://user@updates.example/feed",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
            "https://updates.example/*",
        ),
        (
            "https://updates.example/feed#fragment",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
            "https://updates.example/*",
        ),
        (
            "https://updates.example/feed?channel=stable#fragment",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
            "https://updates.example/*",
        ),
        (
            "https://updates.example/feed",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA",
            "https://updates.example/*",
        ),
        (
            "https://updates.example/feed",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAB=",
            "https://updates.example/*",
        ),
        (
            "https://updates.example/feed",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAB=",
            "https://updates.example/*",
        ),
        (
            "https://other.example/feed",
            "AAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAAA=",
            "https://updates.example/*",
        ),
    ] {
        let source =
            MINIMAL.replace(
                "        http: []",
                &format!("        http: [\n            :: {scope}\n        ]"),
            ) + &format!("\nupdater: {{\n    endpoint:: {endpoint}\n    publicKey:: {key}\n}}\n");
        assert_eq!(
            Manifest::from_ktav_str(&source).unwrap_err().code,
            ErrorCode::ManifestInvalid
        );
    }
}
