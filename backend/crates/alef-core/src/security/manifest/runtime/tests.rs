// SPDX-License-Identifier: MIT OR Apache-2.0
use crate::{
    security::manifest::{runtime_requirement_met, validate_package_version, Manifest},
    ErrorCode,
};

const MINIMAL: &str = include_str!("../../../../tests/fixtures/minimal.ktav");
const VALID: &[&str] = &[
    "^0.1",
    "~0.1.2",
    ">=0.1, <0.2",
    "0.1.*",
    "=0.1.2",
    "0.1.2",
    "*",
    "x",
    "X",
    "1.x",
    "1.X",
    "1.*.*",
    "1",
    "1.2",
    "~1",
    "^0.0.3",
    ">1.2.3",
    ">=1.2.3",
    "<1.2.3",
    "<=1.2.3",
    "=1.2.3-alpha.1",
    "=1.2.3+build.01",
    " >= 1.2.3 , < 2.0.0 ",
    ">=2, <1",
];
const INVALID: &[&str] = &[
    "",
    " ",
    "\n",
    "^",
    "~",
    "=",
    ">=",
    "||",
    "^0.1 || ^0.2",
    ">=0.1 <0.2",
    "1.2.3 - 2.0.0",
    "v1.2.3",
    "latest",
    "1.2.3.4",
    "01.2.3",
    "1.02.3",
    "1.2.03",
    "1.2.3-01",
    "1.2.3-",
    "1.2.3+",
    "1.2.3-alpha..1",
    "1.2.3+build..1",
    "1.2.3-é",
    "1.2.3\0",
    "1.*.3",
    "*, >=1",
    ">=1,",
    ",>=1",
    ">=18446744073709551616.0.0",
];

fn source(value: &str) -> String {
    format!("## runtime: decoy\n\nruntime: {value}\n{MINIMAL}")
}
fn rejected(value: &str) {
    let error = Manifest::from_ktav_str(&source(value)).expect_err("runtime must be rejected");
    assert_eq!(error.code, ErrorCode::ManifestInvalid);
    assert!(error.message.starts_with("runtime:"), "{error:?}");
    assert!(error.message.contains("line 3"), "{error:?}");
    assert_eq!(
        error.details,
        Some(serde_json::json!({"path": "runtime", "line": 3})),
        "{value:?}"
    );
}

#[test]
fn absent_runtime_preserves_serialization_and_directory_versions() {
    let manifest = Manifest::from_ktav_str(MINIMAL).unwrap();
    assert_eq!(manifest.runtime, None);
    let json = serde_json::to_value(&manifest).unwrap();
    assert!(json.get("runtime").is_none());
    assert_eq!(serde_json::from_value::<Manifest>(json).unwrap(), manifest);
    let text = ktav::to_string(&manifest).unwrap();
    assert!(!text.contains("runtime:"));
    assert_eq!(Manifest::from_ktav_str(&text).unwrap(), manifest);
    for version in ["release-candidate", "1", "1.2", "v1.2.3", "01.2.3"] {
        let mut json = serde_json::to_value(&manifest).unwrap();
        json["version"] = version.into();
        let text = ktav::to_string(&json).unwrap();
        assert_eq!(Manifest::from_ktav_str(&text).unwrap().version, version);
        assert!(validate_package_version(version).is_err(), "{version}");
    }
    for version in ["", " "] {
        let mut json = serde_json::to_value(&manifest).unwrap();
        json["version"] = version.into();
        let error = Manifest::from_ktav_str(&ktav::to_string(&json).unwrap()).unwrap_err();
        assert_eq!(error.details, Some(serde_json::json!({"path": "version"})));
    }
}

#[test]
fn accepted_requirements_preserve_exact_strings_in_json_and_ktav_round_trips() {
    for requirement in VALID {
        let manifest = Manifest::from_ktav_str(&source(&format!("((\n{requirement}\n))"))).unwrap();
        assert_eq!(manifest.runtime.as_deref(), Some(*requirement));
        let json = serde_json::to_value(&manifest).unwrap();
        assert_eq!(json["runtime"], *requirement);
        assert_eq!(serde_json::from_value::<Manifest>(json).unwrap(), manifest);
        assert_eq!(
            Manifest::from_ktav_str(&ktav::to_string(&manifest).unwrap()).unwrap(),
            manifest
        );
    }
}

#[test]
fn rejected_requirements_fail_all_deserialization_paths_and_the_match_helper() {
    for requirement in INVALID {
        rejected(&format!("((\n{requirement}\n))"));
        let mut json = serde_json::to_value(Manifest::from_ktav_str(MINIMAL).unwrap()).unwrap();
        json["runtime"] = (*requirement).into();
        assert!(serde_json::from_value::<Manifest>(json)
            .unwrap_err()
            .to_string()
            .starts_with("runtime:"));
        assert!(
            !runtime_requirement_met(requirement, "1.2.3"),
            "{requirement:?}"
        );
    }
}

#[test]
fn null_and_every_nonstring_shape_are_rejected_without_ktav_coercion() {
    for value in [
        "null",
        "true",
        "false",
        "123",
        "1.5",
        "[]",
        "{}",
        "[\n:: ^0.1\n]",
        "{range: ^0.1}",
    ] {
        rejected(value);
    }
    for value in [
        serde_json::Value::Null,
        serde_json::json!(true),
        serde_json::json!(false),
        serde_json::json!(123),
        serde_json::json!(1.5),
        serde_json::json!([]),
        serde_json::json!({}),
        serde_json::json!(["^0.1"]),
        serde_json::json!({"range": "^0.1"}),
    ] {
        let mut json = serde_json::to_value(Manifest::from_ktav_str(MINIMAL).unwrap()).unwrap();
        json["runtime"] = value;
        assert!(serde_json::from_value::<Manifest>(json)
            .unwrap_err()
            .to_string()
            .starts_with("runtime:"));
    }
    assert_eq!(
        Manifest::from_ktav_str(&format!("runtime:: 123\n{MINIMAL}"))
            .unwrap()
            .runtime
            .as_deref(),
        Some("123")
    );
}

#[test]
fn runtime_diagnostics_locate_keys_not_comments_or_nested_decoys() {
    for key in [
        "runtime",
        "\"runtime\"",
        "'runtime'",
        "`runtime`",
        "\"run\\u0074ime\"",
    ] {
        let text = format!("## runtime: decoy\n\n{key}: null\n{MINIMAL}");
        for src in [text.clone(), text.replace('\n', "\r\n")] {
            let error = Manifest::from_ktav_str(&src).unwrap_err();
            assert_eq!(
                error.details,
                Some(serde_json::json!({"path": "runtime", "line": 3})),
                "{key}: {error:?}"
            );
        }
    }
    let text = format!(
        "{{\nname: ((\nruntime: decoy\n))\nruntime: null\n{}\n}}",
        MINIMAL
            .lines()
            .filter(|line| !line.starts_with("name:"))
            .collect::<Vec<_>>()
            .join("\n")
    );
    assert_eq!(
        Manifest::from_ktav_str(&text).unwrap_err().details,
        Some(serde_json::json!({"path": "runtime", "line": 5}))
    );
}

#[test]
fn matching_obeys_cargo_boundaries_not_npm_or_lexical_comparisons() {
    for (requirement, yes, no) in [
        ("^0.1", "0.1.99", "0.2.0"),
        ("^0.0.3", "0.0.3", "0.0.4"),
        ("^1.2.3", "1.99.0", "2.0.0"),
        ("~1.2.3", "1.2.99", "1.3.0"),
        (">=0.1, <0.2", "0.1.0", "0.2.0"),
        ("0.1.*", "0.1.8", "0.2.0"),
        ("=0.1.2", "0.1.2", "0.1.3"),
        ("0.1.2", "0.1.3", "0.2.0"),
        (">1.2.3", "1.2.4", "1.2.3"),
        (">=1.2.3", "1.2.3", "1.2.2"),
        ("<1.2.3", "1.2.2", "1.2.3"),
        ("<=1.2.3", "1.2.3", "1.2.4"),
        (">=1.2.9", "1.2.10", "1.2.8"),
        ("=1.2", "1.2.99", "1.3.0"),
        ("*", "99.0.0", "1.2.3-alpha"),
        (">=1.2.3-alpha.1, <2", "1.2.3-alpha.2", "1.2.4-alpha.2"),
        ("=1.2.3+one", "1.2.3+two", "1.2.4+one"),
    ] {
        assert!(
            runtime_requirement_met(requirement, yes),
            "{requirement} / {yes}"
        );
        assert!(
            !runtime_requirement_met(requirement, no),
            "{requirement} / {no}"
        );
    }
    assert!(!runtime_requirement_met(">=2, <1", "1.5.0"));
    assert!(!runtime_requirement_met("^0.1", "0.0.99"));
    assert!(!runtime_requirement_met("^1.2.3", "1.2.3-alpha"));
}

#[test]
fn package_versions_are_strict_and_invalid_runtime_versions_never_match() {
    for version in [
        "0.0.0",
        "1.2.3",
        "1.2.3-alpha.1",
        "1.2.3+build.01",
        "1.2.3-alpha.1+build.01",
        "18446744073709551615.0.0",
    ] {
        assert_eq!(
            validate_package_version(version).unwrap().to_string(),
            version
        );
    }
    for version in [
        "",
        " ",
        "1",
        "1.2",
        "v1.2.3",
        "=1.2.3",
        "^1.2.3",
        "1.2.*",
        "01.2.3",
        "1.02.3",
        "1.2.03",
        "1.2.3-01",
        "1.2.3-",
        "1.2.3+",
        "1.2.3-alpha..1",
        "1.2.3+build..1",
        "1.2.3-é",
        "1.2.3\0",
        " 1.2.3",
        "1.2.3 ",
        "1.2.3.4",
        "18446744073709551616.0.0",
    ] {
        assert!(validate_package_version(version).is_err(), "{version:?}");
        assert!(!runtime_requirement_met("*", version), "{version:?}");
    }
}

#[test]
fn other_unknown_fields_wrong_types_and_duplicate_runtime_remain_errors() {
    for extra in [
        "runtim: ^0.1",
        "runtimeVersion: 0.1.0",
        "console: []",
        "external.runtime: ^0.1",
        "runtime: ^0.1\nruntime: ^0.2",
    ] {
        assert_eq!(
            Manifest::from_ktav_str(&format!("{MINIMAL}\n{extra}\n"))
                .unwrap_err()
                .code,
            ErrorCode::ManifestInvalid
        );
    }
    let json = serde_json::to_string(&Manifest::from_ktav_str(MINIMAL).unwrap()).unwrap();
    let duplicate = format!("{{\"runtime\":\"^0.1\",\"runtime\":\"^0.2\",{}", &json[1..]);
    assert!(serde_json::from_str::<Manifest>(&duplicate).is_err());
}
