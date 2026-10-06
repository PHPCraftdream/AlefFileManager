// SPDX-License-Identifier: MIT OR Apache-2.0
use alef_core::security::csp::build_csp;
use alef_core::security::manifest::{External, ExternalLoad};
use alef_core::ErrorCode;

fn external() -> External {
    External {
        connect: vec![],
        load: ExternalLoad {
            scripts: vec![],
            styles: vec![],
            images: vec![],
            fonts: vec![],
            media: vec![],
            frames: vec![],
        },
    }
}

#[test]
fn empty_external_is_exact_baseline() {
    assert_eq!(build_csp(&external(), "https://app.example.alef").unwrap(), "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self'; font-src 'self'; media-src 'self'; frame-src 'self'; connect-src 'self' native:; base-uri 'none'; object-src 'none'; form-action 'none'");
}

#[test]
fn load_entries_are_scoped_and_normalized() {
    let mut policy = external();
    policy.load.scripts.push("https://API.example.COM".into());
    policy.load.styles.push("https://style.example".into());
    policy.load.images.push("data:".into());
    assert_eq!(build_csp(&policy, "https://app.example.alef").unwrap(), "default-src 'none'; script-src 'self' https://api.example.com; style-src 'self' https://style.example; img-src 'self' data:; font-src 'self'; media-src 'self'; frame-src 'self'; connect-src 'self' native:; base-uri 'none'; object-src 'none'; form-action 'none'");
}

#[test]
fn connections_are_stable_and_deduplicated() {
    let mut policy = external();
    policy.connect = vec!["https://a.example".into(), "https://a.example".into()];
    let expected = "default-src 'none'; script-src 'self'; style-src 'self'; img-src 'self'; font-src 'self'; media-src 'self'; frame-src 'self'; connect-src 'self' native: https://a.example; base-uri 'none'; object-src 'none'; form-action 'none'";
    assert_eq!(
        build_csp(&policy, "https://app.example.alef").unwrap(),
        expected
    );
    assert_eq!(
        build_csp(&policy, "https://app.example.alef").unwrap(),
        expected
    );
}

#[test]
fn valid_borderline_entries_and_app_origin() {
    for entry in [
        "https://*.example.com",
        "wss://ws.example.com:8443",
        "http://localhost",
        "http://127.0.0.1:8080",
        "http://[::1]:9000",
        "data:",
        "blob:",
    ] {
        let mut policy = external();
        policy.connect.push(entry.into());
        let output = build_csp(&policy, "https://app.example.alef").unwrap();
        assert!(output.contains("connect-src 'self' native:"));
        assert!(!output.contains("https://app.example.alef"));
    }
}

#[test]
fn hostile_entries_are_rejected_with_context() {
    for entry in [
        "'unsafe-inline'",
        "'unsafe-eval'",
        "'self'",
        "*",
        "https://*",
        "javascript:alert(1)",
        "https://a.com;script-src *",
        "https://a.com,https://b.com",
        "https://a.com ",
        "https://a.com\n",
        "ws://insecure.example",
        "ftp://x",
        "file:///etc/passwd",
        "data:text/html;base64,PHNjcg==",
        "https://a..b",
        "https://a.com:0",
        "https://a.com:99999",
        "https://a.com/path",
        "http://external.example",
        "ht tps://a.com",
        "https://a.com%00",
    ] {
        let mut policy = external();
        policy.connect.push(entry.into());
        let error = build_csp(&policy, "https://app.example.alef").unwrap_err();
        assert_eq!(error.code, ErrorCode::ManifestInvalid, "{entry}");
        assert!(error.message.contains(entry), "{entry}: {}", error.message);
    }
}

#[test]
fn app_origin_is_validated_but_not_emitted() {
    let output = build_csp(&external(), "https://app.example.alef").unwrap();
    assert!(!output.contains("https://app.example.alef"));
    for origin in ["https://a.com;script-src *", "http://a.com"] {
        assert_eq!(
            build_csp(&external(), origin).unwrap_err().code,
            ErrorCode::ManifestInvalid
        );
    }
}

#[test]
fn restrictive_directives_remain_for_populated_policy() {
    let mut policy = external();
    policy.load.scripts.push("https://script.example".into());
    let output = build_csp(&policy, "https://app.example.alef").unwrap();
    for directive in [
        "default-src 'none'",
        "base-uri 'none'",
        "object-src 'none'",
        "form-action 'none'",
    ] {
        assert!(output.contains(directive));
    }
}

#[test]
fn a_native_app_origin_is_emitted_because_self_cannot_match_it() {
    let mut policy = external();
    policy.load.scripts.push("https://cdn.example".into());
    policy.connect.push("https://api.example".into());
    assert_eq!(
        build_csp(&policy, "native://app").unwrap(),
        "default-src 'none'; script-src 'self' native://app https://cdn.example; style-src 'self' native://app; img-src 'self' native://app; font-src 'self' native://app; media-src 'self' native://app; frame-src 'self' native://app; connect-src 'self' native: https://api.example; base-uri 'none'; object-src 'none'; form-action 'none'"
    );
}

#[test]
fn only_plain_native_hosts_are_accepted_as_the_app_origin() {
    for bad in [
        "native://",
        "native://App",
        "native://app/",
        "native://app:80",
        "native://a b",
        "native://app;script-src *",
        "native:app",
        "http://app.example",
    ] {
        let error = build_csp(&external(), bad).unwrap_err();
        assert_eq!(error.code, ErrorCode::ManifestInvalid, "{bad}");
    }
}
