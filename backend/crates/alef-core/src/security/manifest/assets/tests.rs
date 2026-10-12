// SPDX-License-Identifier: MIT OR Apache-2.0
use crate::{security::manifest::Manifest, ErrorCode};

const MINIMAL: &str = include_str!("../../../../tests/fixtures/minimal.ktav");

fn source(value: &str) -> String {
    format!("## assets: decoy\n\nassets: {value}\n{MINIMAL}")
}
fn rejected(value: &str) {
    let error = Manifest::from_ktav_str(&source(value)).expect_err("assets must be rejected");
    assert_eq!(error.code, ErrorCode::ManifestInvalid, "{value:?}");
    assert!(error.message.contains("assets"), "{error:?}");
    assert!(error.message.contains("line 3"), "{error:?}");
    assert_eq!(
        error.details,
        Some(serde_json::json!({"path": "assets", "line": 3})),
        "{value:?}"
    );
}

#[test]
fn omitted_assets_remain_absent_instead_of_materializing_the_default_root() {
    let manifest = Manifest::from_ktav_str(MINIMAL).unwrap();
    assert_eq!(manifest.assets, None);
    let json = serde_json::to_value(&manifest).unwrap();
    assert!(json.get("assets").is_none());
    assert_eq!(serde_json::from_value::<Manifest>(json).unwrap(), manifest);
    assert_eq!(
        Manifest::from_ktav_str(&ktav::to_string(&manifest).unwrap()).unwrap(),
        manifest
    );
}

#[test]
fn ordinary_asset_subdirectories_preserve_their_spelling_across_round_trips() {
    for path in [
        "dist",
        "web/build",
        "A-z_09.@+/.hidden",
        "COM0/LPT10",
        "conifer/nulled",
        "META-INF-extra",
        "web/META-INF",
        "123",
    ] {
        let manifest = Manifest::from_ktav_str(&source(&format!("((\n{path}\n))"))).unwrap();
        assert_eq!(manifest.assets.as_deref(), Some(path));
        let json = serde_json::to_value(&manifest).unwrap();
        assert_eq!(json["assets"], path);
        assert_eq!(serde_json::from_value::<Manifest>(json).unwrap(), manifest);
        assert_eq!(
            Manifest::from_ktav_str(&ktav::to_string(&manifest).unwrap()).unwrap(),
            manifest
        );
    }
    assert_eq!(
        Manifest::from_ktav_str(&source("web/build"))
            .unwrap()
            .assets
            .as_deref(),
        Some("web/build")
    );
}

#[test]
fn invalid_asset_components_cannot_select_the_application_root_or_path_aliases() {
    for path in [
        "",
        ".",
        "..",
        "/dist",
        "//host/share",
        "dist/",
        "web//build",
        "./dist",
        "web/../dist",
        "web/./dist",
        "C:/dist",
        "C:dist",
        "dist:stream",
        "dist\\sub",
        "\\\\host\\share",
        "dist.",
        "web./dist",
        "dist ",
        "web /dist",
        "a b",
        "a*b",
        "a?b",
        "a<b",
        "a>b",
        "a|b",
        "a%b",
        "a~b",
        "café",
        "אב",
        "META-INF",
        "meta-inf/public",
        "MeTa-InF/hashes",
    ] {
        // Verbatim multiline scalars preserve trailing spaces and backslashes.
        rejected(&format!("((\n{path}\n))"));
    }
    for control in ['\0', '\u{1}', '\u{1f}', '\u{7f}', '\u{85}'] {
        rejected(&format!("((\nweb{control}/build\n))"));
    }
}

#[test]
fn windows_devices_are_rejected_in_every_component_even_with_extensions() {
    for stem in ["CON", "PRN", "AUX", "NUL"] {
        rejected(&format!("web/{}.txt", stem.to_ascii_lowercase()));
        rejected(stem);
    }
    for prefix in ["COM", "LPT"] {
        for digit in ["1", "2", "3", "4", "5", "6", "7", "8", "9", "¹", "²", "³"] {
            rejected(&format!("web/{prefix}{digit}.txt"));
            rejected(&format!("{}{digit}", prefix.to_ascii_lowercase()));
        }
    }
}

#[test]
fn format_one_component_and_path_limits_accept_the_boundary_and_reject_one_more_byte() {
    let part = "a".repeat(255);
    let path = format!("{}/{part}/{part}/{part}/a", "a".repeat(254));
    assert_eq!(path.len(), 1024);
    for valid in [&part, &path] {
        assert_eq!(
            Manifest::from_ktav_str(&source(valid))
                .unwrap()
                .assets
                .as_ref(),
            Some(valid)
        );
    }
    rejected(&"a".repeat(256));
    rejected(&format!("{path}a"));
}

#[test]
fn present_asset_values_must_be_strings_rather_than_null_numbers_or_compounds() {
    for value in [
        "null",
        "true",
        "false",
        "123",
        "1.5",
        "[]",
        "{}",
        "[dist]",
        "{root: dist}",
    ] {
        rejected(value);
        let mut json = serde_json::to_value(Manifest::from_ktav_str(MINIMAL).unwrap()).unwrap();
        json["assets"] = serde_json::from_str(value).unwrap_or(serde_json::json!({"root": "dist"}));
        assert!(serde_json::from_value::<Manifest>(json).is_err());
    }
}

#[test]
fn asset_diagnostics_identify_the_actual_key_line_in_supported_document_spellings() {
    for key in ["assets", "\"assets\"", "'assets'", "`assets`"] {
        let text = format!("## assets: decoy\n\n{key}: ../private\n{MINIMAL}");
        for src in [text.clone(), text.replace('\n', "\r\n")] {
            let error = Manifest::from_ktav_str(&src).unwrap_err();
            assert_eq!(
                error.details,
                Some(serde_json::json!({"path": "assets", "line": 3})),
                "{key}: {error:?}"
            );
        }
    }
    let text = format!("{{\nassets: null\n{MINIMAL}}}\n");
    assert_eq!(
        Manifest::from_ktav_str(&text).unwrap_err().details,
        Some(serde_json::json!({"path": "assets", "line": 2}))
    );
}

#[test]
fn unrelated_unknown_fields_and_wrong_types_keep_their_existing_rejections() {
    for extra in [
        "asset: dist",
        "assetsRoot: dist",
        "console: []",
        "external.assets: dist",
    ] {
        let error = Manifest::from_ktav_str(&format!("{MINIMAL}\n{extra}\n")).unwrap_err();
        assert_eq!(error.code, ErrorCode::ManifestInvalid);
        assert!(!error.message.starts_with("assets:"), "{error:?}");
    }
}
