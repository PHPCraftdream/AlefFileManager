// SPDX-License-Identifier: MIT OR Apache-2.0
use alef_core::{
    security::{
        manifest::{ArgKind, Manifest},
        window::{Length, LengthUnit, Monitor, WindowPosition},
    },
    ErrorCode,
};
use std::str::FromStr;

const MINIMAL: &str = include_str!("fixtures/minimal.ktav");
const OPEN: &str = include_str!("fixtures/open.ktav");

fn error(src: &str) -> alef_core::AlefError {
    let err = Manifest::from_ktav_str(src).expect_err("manifest must be rejected");
    assert_eq!(err.code, ErrorCode::ManifestInvalid);
    err
}

#[test]
fn crlf_manifest_parses_like_lf() {
    let lf = Manifest::from_ktav_str(MINIMAL).unwrap();
    let crlf = Manifest::from_ktav_str(&MINIMAL.replace('\n', "\r\n")).unwrap();
    assert_eq!(crlf, lf);
}

#[test]
fn minimal_fixture_asserts_closed_schema_exactly() {
    let m = Manifest::from_ktav_str(MINIMAL).unwrap();
    assert_eq!(m.id, "org.example.app");
    assert_eq!(m.name, "Example");
    assert_eq!(m.version, "1.0.0");
    assert_eq!(m.external.connect, Vec::<String>::new());
    assert_eq!(m.external.load.scripts, Vec::<String>::new());
    assert_eq!(m.external.load.styles, Vec::<String>::new());
    assert_eq!(m.external.load.images, Vec::<String>::new());
    assert_eq!(m.external.load.fonts, Vec::<String>::new());
    assert_eq!(m.external.load.media, Vec::<String>::new());
    assert_eq!(m.external.load.frames, Vec::<String>::new());
    assert_eq!(m.permissions.fs.read, Vec::<String>::new());
    assert_eq!(m.permissions.fs.write, Vec::<String>::new());
    assert_eq!(m.permissions.cli.exec, Vec::<String>::new());
    assert_eq!(m.permissions.net.http, Vec::<String>::new());
    assert_eq!(m.permissions.net.socket, Vec::<String>::new());
    assert_eq!(m.permissions.shell.open_external, Vec::<String>::new());
    assert!(!m.permissions.clipboard.read);
    assert!(!m.permissions.shortcut.global);
    assert!(!m.permissions.secrets);
    assert_eq!(m.permissions.app.env, Vec::<String>::new());
    assert_eq!(m.windows.len(), 1);
    assert_eq!(m.windows[0].label, "main");
    assert_eq!(m.windows[0].url, "/");
    assert_eq!(m.windows[0].width, Length::Px(800.0));
    assert_eq!(m.windows[0].height, Length::Px(600.0));
}

#[test]
fn open_fixture_asserts_policy_and_window() {
    let m = Manifest::from_ktav_str(OPEN).unwrap();
    assert_eq!(m.windows[0].width, Length::Percent(70.0, LengthUnit::Work));
    assert_eq!(m.windows[0].height, Length::Percent(80.0, LengthUnit::Work));
    assert_eq!(m.windows[0].monitor, Monitor::Cursor);
    assert_eq!(m.windows[0].position, WindowPosition::Center);
    assert!(m.windows[0].restore);
    assert_eq!(m.external.connect, vec!["https://api.example.com"]);
    assert_eq!(m.permissions.fs.read, vec!["/notes"]);
    assert_eq!(m.permissions.cli.exec, vec!["git"]);
}

#[test]
fn security_sections_and_nested_fs_are_required() {
    for (needle, name) in [
        ("external: {", "external"),
        ("permissions: {", "permissions"),
        ("    fs: {", "fs"),
    ] {
        let pos = MINIMAL.find(needle).unwrap();
        let mut s = MINIMAL.to_string();
        let open = pos + needle.len() - 1;
        let mut depth = 0usize;
        let mut end = open;
        for (offset, ch) in s[open..].char_indices() {
            if ch == '{' {
                depth += 1;
            }
            if ch == '}' {
                depth -= 1;
                if depth == 0 {
                    end = open + offset + 1;
                    break;
                }
            }
        }
        s.replace_range(pos..end, "");
        let e = error(&s);
        assert!(e.message.contains(name), "{}", e.message);
    }
}

#[test]
fn unknown_fields_are_denied_at_each_level() {
    let top = MINIMAL.replace("name: Example", "name: Example\nunknownTop: true");
    assert!(error(&top).message.contains("unknown field"));
    let nested = MINIMAL.replace(
        "        frames: []",
        "        frames: []\n        bogus: []",
    );
    assert!(error(&nested).message.contains("bogus"));
}

#[test]
fn wrong_types_and_version_inference_are_reported() {
    // the whole array is replaced, so the failure can only come from the type of `windows`
    let start = MINIMAL.find("windows: [").unwrap();
    let end = MINIMAL[start..].find("]\nexternal").unwrap() + start + 1;
    let mut scalar = MINIMAL.to_string();
    scalar.replace_range(start..end, "windows: 3");
    let wrong_windows = error(&scalar);
    assert!(
        wrong_windows.message.contains("expected array"),
        "{}",
        wrong_windows.message
    );
    assert!(error(&MINIMAL.replace("width: 800", "width: abc"))
        .message
        .contains("length"));
    let unquoted_float =
        Manifest::from_ktav_str(&MINIMAL.replace("version:: 1.0.0", "version: 1.0")).unwrap();
    assert_eq!(unquoted_float.version, "1.0");
    let v = Manifest::from_ktav_str(&MINIMAL.replace("version:: 1.0.0", "version: 1.0.0")).unwrap();
    assert_eq!(v.version, "1.0.0");
    let restore = OPEN.replace("restore: true", "restore: yes");
    let restore_error = error(&restore);
    assert!(!restore_error.message.is_empty());
}

#[test]
fn length_parsing_valid_and_invalid_matrix() {
    for (text, expected) in [
        ("0", Length::Px(0.0)),
        ("999999", Length::Px(999999.0)),
        ("1%screen", Length::Percent(1.0, LengthUnit::Screen)),
        ("100%screen", Length::Percent(100.0, LengthUnit::Screen)),
        ("1%work", Length::Percent(1.0, LengthUnit::Work)),
        ("100%work", Length::Percent(100.0, LengthUnit::Work)),
    ] {
        assert_eq!(Length::from_str(text).unwrap(), expected);
    }
    for text in [
        "0%work",
        "101%screen",
        "-5",
        "50%height",
        "50 %work",
        "abc",
        "",
        "nan",
        "inf",
    ] {
        let e = Length::from_str(text).unwrap_err();
        assert_eq!(e.code, ErrorCode::ManifestInvalid, "{text}");
    }
    for text in ["-5", "nan", "inf", "0%work", "50%height"] {
        let s = MINIMAL.replace("width: 800", &format!("width: {text}"));
        assert_eq!(error(&s).code, ErrorCode::ManifestInvalid);
    }
}

#[test]
fn length_display_round_trips() {
    for value in [Length::Px(800.0), Length::Percent(70.0, LengthUnit::Work)] {
        assert_eq!(Length::from_str(&value.to_string()).unwrap(), value);
    }
}

#[test]
fn labels_must_be_nonempty_and_unique() {
    let mut s = MINIMAL.to_string();
    let item = "\n    {\n        label: main\n        url: /\n        width: 800\n        height: 600\n    }";
    let end = s.find("\n]\nexternal").unwrap();
    s.insert_str(end, item);
    assert!(error(&s).message.contains("label"));
    let empty = MINIMAL.replace("label: main", "label:");
    assert!(error(&empty).message.contains("label"));
}

#[test]
fn identifiers_are_validated() {
    for id in ["a..b", ".a", "a.", "a b", ""] {
        let s = MINIMAL.replace("org.example.app", id);
        assert!(error(&s).message.contains("id"));
    }
    let m =
        Manifest::from_ktav_str(&MINIMAL.replace("org.example.app", "org.example-app2")).unwrap();
    assert_eq!(m.id, "org.example-app2");
}

#[test]
fn url_must_be_root_relative() {
    assert!(error(&MINIMAL.replace("url: /", "url: home"))
        .message
        .contains("windows[0].url"));
}

#[test]
fn minimum_maximum_comparison_observes_units() {
    let same = MINIMAL.replace(
        "height: 600",
        "height: 600\n        minWidth: 800\n        maxWidth: 700",
    );
    assert!(error(&same).message.contains("minwidth"));
    let cross = MINIMAL.replace(
        "height: 600",
        "height: 600\n        minWidth: 80%work\n        maxWidth: 70%screen",
    );
    assert_eq!(
        Manifest::from_ktav_str(&cross).unwrap().windows[0].max_width,
        Some(Length::Percent(70.0, LengthUnit::Screen))
    );
}

#[test]
fn structured_error_reports_line_and_column() {
    let src = "id: x\nname: n\nport:8080\n";
    let e = error(src);
    assert_eq!(e.details.unwrap(), serde_json::json!({"line":3,"column":5}));
    assert!(e.message.contains("line 3"));
}

#[test]
fn empty_windows_and_literal_version_parse() {
    let empty = MINIMAL.to_string();
    let start = empty.find("windows: [").unwrap();
    let end = empty[start..].find("]\nexternal").unwrap() + start + 1;
    let mut s = empty;
    s.replace_range(start..end, "windows: []");
    assert_eq!(Manifest::from_ktav_str(&s).unwrap().windows, Vec::new());
    assert!(
        MINIMAL.contains("version:: 1.0.0"),
        "fixture uses the literal form"
    );
    assert_eq!(Manifest::from_ktav_str(MINIMAL).unwrap().version, "1.0.0");
}

fn with_position(position: &str) -> String {
    MINIMAL.replace(
        "height: 600",
        &format!("height: 600\n        position: {position}"),
    )
}

#[test]
fn window_position_is_strict_and_round_trips() {
    assert_eq!(
        Manifest::from_ktav_str(&with_position("center"))
            .unwrap()
            .windows[0]
            .position,
        WindowPosition::Center
    );
    let at = Manifest::from_ktav_str(&with_position(
        "{\n            x: 10\n            y: 5%work\n        }",
    ))
    .unwrap();
    let expected = WindowPosition::At {
        x: Length::Px(10.0),
        y: Length::Percent(5.0, LengthUnit::Work),
    };
    assert_eq!(at.windows[0].position, expected);
    for position in [WindowPosition::Center, expected] {
        let json = serde_json::to_value(&position).unwrap();
        let back: WindowPosition = serde_json::from_value(json.clone()).unwrap();
        assert_eq!(back, position, "{json}");
    }
    assert_eq!(
        serde_json::to_value(WindowPosition::Center).unwrap(),
        "center"
    );
    assert!(
        serde_json::from_value::<WindowPosition>(serde_json::json!({"x": 1, "y": 2, "z": 3}))
            .is_err()
    );
    assert!(serde_json::from_value::<WindowPosition>(serde_json::json!({"x": 1})).is_err());
    assert!(serde_json::from_value::<WindowPosition>(serde_json::json!("left")).is_err());
    assert!(
        serde_json::from_value::<WindowPosition>(serde_json::json!({"x": "-1", "y": 2})).is_err()
    );
}

#[test]
fn serde_json_length_accepts_numbers_and_strings_and_serializes_as_strings() {
    assert_eq!(
        serde_json::from_value::<Length>(serde_json::json!(800)).unwrap(),
        Length::Px(800.0)
    );
    assert_eq!(
        serde_json::from_value::<Length>(serde_json::json!("70%work")).unwrap(),
        Length::Percent(70.0, LengthUnit::Work)
    );
    assert_eq!(serde_json::to_value(Length::Px(800.0)).unwrap(), "800");
    assert_eq!(
        serde_json::to_value(Length::Percent(70.0, LengthUnit::Work)).unwrap(),
        "70%work"
    );
}

#[test]
fn generated_manifest_types_include_window_schema() {
    let types = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("../../../packages/api/types/manifest.ts"),
    )
    .expect("failed to read TypeScript manifest types");
    for declaration in [
        "export type Length = number | string;",
        "export type WindowPosition = \"center\" | { x: Length, y: Length };",
        "export type Monitor = \"primary\" | \"cursor\";",
    ] {
        assert!(types.contains(declaration), "missing {declaration}");
    }
    assert!(types.contains("openExternal"));
    assert!(types.contains("minWidth"));
}

const ARGUMENTS: &str = "arguments: {
    options: [
        {
            name: port
            short: p
            type: number
            description: Port to listen on
        }
        {
            name: verbose
            type: boolean
        }
    ]
    positional: {
        name: files
    }
}
";

fn with_arguments(arguments: &str) -> String {
    format!("{}{arguments}", MINIMAL.replace('\r', ""))
}

#[test]
fn arguments_section_is_optional_and_parses_options_and_positionals() {
    assert_eq!(Manifest::from_ktav_str(MINIMAL).unwrap().arguments, None);
    let manifest = Manifest::from_ktav_str(&with_arguments(ARGUMENTS)).unwrap();
    let arguments = manifest.arguments.expect("arguments");
    assert_eq!(arguments.options.len(), 2);
    assert_eq!(arguments.options[0].name, "port");
    assert_eq!(arguments.options[0].short.as_deref(), Some("p"));
    assert_eq!(arguments.options[0].kind, ArgKind::Number);
    assert_eq!(
        arguments.options[0].description.as_deref(),
        Some("Port to listen on")
    );
    assert_eq!(arguments.options[1].kind, ArgKind::Boolean);
    assert_eq!(arguments.options[1].short, None);
    assert_eq!(arguments.positional.expect("positional").name, "files");
}

#[test]
fn malformed_arguments_are_rejected_with_the_path_of_the_error() {
    let option = |body: &str| {
        format!("arguments: {{\n    options: [\n        {{\n{body}        }}\n    ]\n}}\n")
    };
    for (name, source, path) in [
        (
            "bad name",
            option("            name: Port\n            type: string\n"),
            "arguments.options[0].name",
        ),
        (
            "trailing hyphen",
            option("            name: port-\n            type: string\n"),
            "arguments.options[0].name",
        ),
        (
            "double hyphen",
            option("            name: a--b\n            type: string\n"),
            "arguments.options[0].name",
        ),
        (
            "generated help",
            option("            name: help\n            type: boolean\n"),
            "arguments.options[0].name",
        ),
        (
            "generated version",
            option("            name: version\n            type: boolean\n"),
            "arguments.options[0].name",
        ),
        (
            "long short name",
            option("            name: port\n            short: pp\n            type: string\n"),
            "arguments.options[0].short",
        ),
        (
            "generated -h",
            option("            name: port\n            short: h\n            type: string\n"),
            "arguments.options[0].short",
        ),
        (
            "generated -V",
            option("            name: port\n            short: V\n            type: string\n"),
            "arguments.options[0].short",
        ),
        (
            "non-alphanumeric short",
            option("            name: port\n            short: -\n            type: string\n"),
            "arguments.options[0].short",
        ),
        (
            "unknown type",
            option("            name: port\n            type: integer\n"),
            "unknown variant `integer`",
        ),
    ] {
        let error = error(&with_arguments(&source));
        assert!(
            error.message.contains(path),
            "{name}: expected {path} in {:?}",
            error.message
        );
    }
    let duplicate_name = "arguments: {\n    options: [\n        {\n            name: port\n            type: string\n        }\n        {\n            name: port\n            type: string\n        }\n    ]\n}\n";
    assert!(error(&with_arguments(duplicate_name))
        .message
        .contains("arguments.options[1].name"));
    let duplicate_short = "arguments: {\n    options: [\n        {\n            name: a\n            short: x\n            type: string\n        }\n        {\n            name: b\n            short: x\n            type: string\n        }\n    ]\n}\n";
    assert!(error(&with_arguments(duplicate_short))
        .message
        .contains("arguments.options[1].short"));
    let bad_positional =
        "arguments: {\n    options: []\n    positional: {\n        name: Files\n    }\n}\n";
    assert!(error(&with_arguments(bad_positional))
        .message
        .contains("arguments.positional.name"));
    let unknown_field = "arguments: {\n    options: []\n    bogus: 1\n}\n";
    assert!(error(&with_arguments(unknown_field))
        .message
        .contains("bogus"));
    let missing_options = "arguments: {\n    positional: {\n        name: files\n    }\n}\n";
    assert!(error(&with_arguments(missing_options))
        .message
        .contains("options"));
}
