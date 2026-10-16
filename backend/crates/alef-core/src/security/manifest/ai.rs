// SPDX-License-Identifier: MIT OR Apache-2.0
//! `permissions.ai`: the closed-by-default AI roles and application tools of M9
//! (docs/stages/m9-ai.md §3). Declaring a role or a tool asks for a right of its own; it grants
//! nothing.
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::{AlefError, ErrorCode};

/// AI permissions; closed by default: the whole section is optional and anything not listed
/// is refused.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(export, export_to = "manifest.ts")]
pub struct AiPermissions {
    /// Rush role ids the application may ask for (`ai.role`), in manifest order.
    pub roles: Vec<String>,
    /// Tools of the application the model may call (`ai.tool`), in manifest order.
    pub tools: Vec<AiTool>,
    /// Builtin capabilities delegated to the model.
    #[serde(default)]
    pub builtin: Vec<AiBuiltin>,
}

/// A builtin capability and its optional filesystem scopes.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(export, export_to = "manifest.ts")]
pub struct AiBuiltin {
    /// One of the supported builtin capability names.
    pub name: String,
    /// Filesystem path scopes; must be empty for execution capabilities.
    #[serde(default)]
    pub scopes: Vec<String>,
}

/// A tool the application declares for the model: name, description and a JSON schema of the
/// arguments, restricted to the subset of docs/stages/m9-ai.md §3.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(export, export_to = "manifest.ts")]
pub struct AiTool {
    /// ASCII name matching `[a-zA-Z][a-zA-Z0-9_]{0,63}`; unique among tools.
    pub name: String,
    /// Non-empty text of at most 512 bytes, shown to the user.
    pub description: String,
    /// Argument schema in the allowed JSON Schema subset.
    #[ts(type = "unknown")]
    pub schema: Value,
}

/// Role and tool ids share the shape; the two namespaces are separate.
fn valid_name(name: &str) -> bool {
    let bytes = name.as_bytes();
    (1..=64).contains(&bytes.len())
        && bytes[0].is_ascii_alphabetic()
        && bytes[1..]
            .iter()
            .all(|b| b.is_ascii_alphanumeric() || *b == b'_')
}

/// Reject broad or parent-traversing roots before the permission grammar is parsed.
fn specific_scope(scope: &str) -> bool {
    let normalized = scope.replace('\\', "/");
    let parts: Vec<&str> = normalized
        .split('/')
        .filter(|part| !part.is_empty() && *part != ".")
        .collect();
    let drive_prefix = parts.first().is_some_and(|part| part.ends_with(':'));
    let variable_prefix = parts.first().is_some_and(|part| part.starts_with('$'));
    let rooted = normalized.starts_with('/') || drive_prefix;
    if parts.iter().any(|part| *part == "..") {
        return false;
    }
    let base_start = usize::from(drive_prefix || variable_prefix);
    let first_wildcard = parts.iter().position(|part| part.contains('*'));
    if first_wildcard.is_some_and(|index| index < base_start) {
        return false;
    }
    let concrete = parts[base_start..]
        .iter()
        .take_while(|part| !part.contains('*'))
        .count();
    if concrete == 0 && first_wildcard.is_some() {
        return false;
    }
    if variable_prefix {
        concrete >= 1
    } else if rooted {
        concrete >= 2
    } else {
        concrete >= 1
    }
}

/// Schema keywords of the allowed subset; anything else (`$ref` included) is rejected.
const KEYS: [&str; 12] = [
    "type",
    "properties",
    "required",
    "items",
    "enum",
    "additionalProperties",
    "minimum",
    "maximum",
    "minLength",
    "maxLength",
    "minItems",
    "maxItems",
];
const TYPES: [&str; 7] = [
    "object", "string", "number", "integer", "boolean", "null", "array",
];
const MAX_DEPTH: usize = 32;
const MAX_PROPERTIES: usize = 256;
const MAX_ENUM: usize = 64;
const MAX_ITEMS: i64 = 4096;
const MAX_LENGTH: i64 = 65536;
const MAX_SCHEMA_BYTES: usize = 64 * 1024;

/// Validates an `ai` section: the names, the tools and every schema.
pub fn validate_ai(ai: &AiPermissions) -> Result<(), AlefError> {
    let invalid = |path: &str, reason: &str| {
        AlefError::new(ErrorCode::ManifestInvalid, format!("{path}: {reason}"))
            .with_details(json!({"path": path}))
    };
    for (index, builtin) in ai.builtin.iter().enumerate() {
        let path = format!("ai.builtin[{index}]");
        if !["fs.read", "fs.write", "fs.list", "exec", "bash"].contains(&builtin.name.as_str()) {
            return Err(invalid(&format!("{path}.name"), "unsupported builtin name"));
        }
        if ai.builtin[..index]
            .iter()
            .any(|other| other.name == builtin.name)
        {
            return Err(invalid(&format!("{path}.name"), "duplicate builtin"));
        }
        let filesystem = builtin.name.starts_with("fs.");
        if filesystem && builtin.scopes.is_empty() {
            return Err(invalid(
                &format!("{path}.scopes"),
                "filesystem builtin requires scopes",
            ));
        }
        if !filesystem && !builtin.scopes.is_empty() {
            return Err(invalid(
                &format!("{path}.scopes"),
                "execution builtin cannot have scopes",
            ));
        }
        for (scope_index, scope) in builtin.scopes.iter().enumerate() {
            if builtin.scopes[..scope_index].contains(scope) {
                return Err(invalid(
                    &format!("{path}.scopes[{scope_index}]"),
                    "duplicate scope",
                ));
            }
            if !specific_scope(scope) {
                return Err(invalid(
                    &format!("{path}.scopes[{scope_index}]"),
                    "scope must be a specific, safe path",
                ));
            }
        }
    }
    for (index, role) in ai.roles.iter().enumerate() {
        if !valid_name(role) {
            return Err(invalid(
                &format!("ai.roles[{index}]"),
                "must be 1–64 ASCII bytes matching [a-zA-Z][a-zA-Z0-9_]{0,63}",
            ));
        }
        if ai.roles[..index].contains(role) {
            return Err(invalid(&format!("ai.roles[{index}]"), "duplicate role"));
        }
    }
    for (index, tool) in ai.tools.iter().enumerate() {
        let path = format!("ai.tools[{index}]");
        if !valid_name(&tool.name) {
            return Err(invalid(
                &format!("{path}.name"),
                "must be 1–64 ASCII bytes matching [a-zA-Z][a-zA-Z0-9_]{0,63}",
            ));
        }
        if ai.tools[..index]
            .iter()
            .any(|other| other.name == tool.name)
        {
            return Err(invalid(&format!("{path}.name"), "duplicate tool"));
        }
        if tool.description.is_empty() {
            return Err(invalid(&format!("{path}.description"), "must not be empty"));
        }
        if tool.description.len() > 512 {
            return Err(invalid(
                &format!("{path}.description"),
                "must be at most 512 bytes",
            ));
        }
        let schema_path = format!("{path}.schema");
        validate_schema(&tool.schema, &schema_path, 0)
            .map_err(|(path, reason)| invalid(&path, &reason))?;
        if serde_json::to_vec(&tool.schema).is_ok_and(|bytes| bytes.len() > MAX_SCHEMA_BYTES) {
            return Err(invalid(&schema_path, "must serialize to at most 64 KiB"));
        }
    }
    Ok(())
}

/// Fails closed: every keyword outside the subset, every unbounded shape and every unsane bound
/// is an error carrying the path of the violation.
fn validate_schema(schema: &Value, path: &str, depth: usize) -> Result<(), (String, String)> {
    let bad = move |at: String, reason: &str| -> Result<(), (String, String)> {
        Err((at, reason.to_owned()))
    };
    if depth > MAX_DEPTH {
        return bad(path.to_owned(), "is nested deeper than 32 levels");
    }
    let Some(object) = schema.as_object() else {
        return bad(path.to_owned(), "must be a JSON object");
    };
    for key in object.keys() {
        if !KEYS.contains(&key.as_str()) {
            return bad(format!("{path}.{key}"), "is not part of the allowed subset");
        }
    }
    let Some(kind) = object.get("type") else {
        return bad(path.to_owned(), "must declare a type");
    };
    let Some(kind) = kind.as_str() else {
        return bad(format!("{path}.type"), "must be a string");
    };
    if !TYPES.contains(&kind) {
        return bad(format!("{path}.type"), "is not one of the allowed types");
    }
    if kind == "object" && object.get("additionalProperties") != Some(&Value::Bool(false)) {
        return bad(
            format!("{path}.additionalProperties"),
            "must be the boolean false",
        );
    }
    if let Some(properties) = object.get("properties") {
        let Some(properties) = properties.as_object() else {
            return bad(format!("{path}.properties"), "must be an object");
        };
        if properties.len() > MAX_PROPERTIES {
            return bad(
                format!("{path}.properties"),
                "must contain at most 256 entries",
            );
        }
        for (name, property) in properties {
            validate_schema(property, &format!("{path}.properties.{name}"), depth + 1)?;
        }
    }
    if let Some(required) = object.get("required") {
        let Some(required) = required.as_array() else {
            return bad(format!("{path}.required"), "must be an array of strings");
        };
        let mut seen = std::collections::HashSet::new();
        for (index, name) in required.iter().enumerate() {
            let Some(name) = name.as_str() else {
                return bad(
                    format!("{path}.required[{index}]"),
                    "must be an array of strings",
                );
            };
            if !seen.insert(name) {
                return bad(
                    format!("{path}.required[{index}]"),
                    "names a property twice",
                );
            }
            if !object
                .get("properties")
                .and_then(Value::as_object)
                .is_some_and(|properties| properties.contains_key(name))
            {
                return bad(
                    format!("{path}.required[{index}]"),
                    "names a property that does not exist",
                );
            }
        }
    }
    if let Some(items) = object.get("items") {
        validate_schema(items, &format!("{path}.items"), depth + 1)?;
    }
    if let Some(enum_values) = object.get("enum") {
        let Some(enum_values) = enum_values.as_array() else {
            return bad(format!("{path}.enum"), "must be a non-empty array");
        };
        if enum_values.is_empty() || enum_values.len() > MAX_ENUM {
            return bad(
                format!("{path}.enum"),
                "must be an array of 1–64 JSON values",
            );
        }
    }
    if let Some(additional) = object.get("additionalProperties") {
        if additional != &Value::Bool(false) {
            return bad(
                format!("{path}.additionalProperties"),
                "must be the boolean false",
            );
        }
    }
    for key in ["minimum", "maximum"] {
        if let Some(bound) = object.get(key) {
            if !bound.is_number() {
                return bad(format!("{path}.{key}"), "must be a number");
            }
        }
    }
    let number = |key: &str| object.get(key).and_then(Value::as_f64);
    if let (Some(minimum), Some(maximum)) = (number("minimum"), number("maximum")) {
        if minimum > maximum {
            return bad(path.to_owned(), "declares a minimum above its maximum");
        }
    }
    for (key, limit) in [
        ("minLength", MAX_LENGTH),
        ("maxLength", MAX_LENGTH),
        ("minItems", MAX_ITEMS),
        ("maxItems", MAX_ITEMS),
    ] {
        if let Some(bound) = object.get(key) {
            match bound.as_i64() {
                Some(bound) if (0..=limit).contains(&bound) => {}
                _ => {
                    return bad(
                        format!("{path}.{key}"),
                        "must be an integer within its allowed range",
                    );
                }
            }
        }
    }
    let bound = |key: &str| object.get(key).and_then(Value::as_i64);
    if let (Some(minimum), Some(maximum)) = (bound("minLength"), bound("maxLength")) {
        if minimum > maximum {
            return bad(path.to_owned(), "declares a minLength above its maxLength");
        }
    }
    if let (Some(minimum), Some(maximum)) = (bound("minItems"), bound("maxItems")) {
        if minimum > maximum {
            return bad(path.to_owned(), "declares a minItems above its maxItems");
        }
    }
    if object.get("type").and_then(Value::as_str) == Some("array") {
        match object.get("maxItems").and_then(Value::as_i64) {
            Some(max_items) if (0..=MAX_ITEMS).contains(&max_items) => {}
            _ => {
                return bad(
                    format!("{path}.maxItems"),
                    "an array must declare an integer maxItems of at most 4096",
                );
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{validate_ai, AiPermissions, AiTool, MAX_SCHEMA_BYTES};
    use crate::security::{
        consent::{Consent, Decision, Right},
        grants::Grants,
        manifest::{
            AppPermissions, CliPermissions, ClipboardPermissions, FsPermissions, Manifest,
            NetPermissions, Permissions, ShellPermissions, ShortcutPermissions,
        },
        permissions::{PathVars, Permission, PermissionSet},
    };
    use crate::ErrorCode;
    use serde_json::{json, Value};
    use std::path::PathBuf;

    const MINIMAL: &str = include_str!("../../../tests/fixtures/minimal.ktav");

    fn error(ai: &AiPermissions) -> crate::AlefError {
        let err = validate_ai(ai).expect_err("section must be rejected");
        assert_eq!(err.code, ErrorCode::ManifestInvalid);
        err
    }

    fn tool(name: &str, schema: Value) -> AiTool {
        AiTool {
            name: name.to_owned(),
            description: "Summarizes text".to_owned(),
            schema,
        }
    }

    fn ai(roles: &[&str], tools: Vec<AiTool>) -> AiPermissions {
        AiPermissions {
            roles: roles.iter().map(|role| role.to_string()).collect(),
            tools,
            builtin: Vec::new(),
        }
    }

    fn simple_tool(name: &str) -> AiTool {
        tool(
            name,
            json!({"type": "object", "properties": {}, "additionalProperties": false}),
        )
    }

    fn valid() -> AiPermissions {
        ai(
            &["smart"],
            vec![tool(
                "summarizeSelection",
                json!({
                    "type": "object",
                    "properties": {"text": {"type": "string"}},
                    "required": ["text"],
                    "additionalProperties": false
                }),
            )],
        )
    }

    fn wrapped(depth: usize, inner: Value) -> Value {
        (0..depth).fold(inner, |schema, _| {
            json!({
                "type": "object",
                "properties": {"a": schema},
                "additionalProperties": false
            })
        })
    }

    fn nested(depth: usize) -> Value {
        wrapped(depth, json!({"type": "string"}))
    }

    fn policy(section: Option<AiPermissions>) -> Permissions {
        Permissions {
            fs: FsPermissions {
                read: Vec::new(),
                write: Vec::new(),
            },
            cli: CliPermissions {
                exec: Vec::new(),
                commands: Vec::new(),
            },
            net: NetPermissions {
                http: Vec::new(),
                socket: Vec::new(),
            },
            shell: ShellPermissions {
                open_external: Vec::new(),
                context_menu: false,
                context_menu_runas: false,
            },
            clipboard: ClipboardPermissions { read: false },
            shortcut: ShortcutPermissions { global: false },
            tray: false,
            secrets: false,
            webcrypto: false,
            app: AppPermissions {
                env: Vec::new(),
                autostart: false,
            },
            window: None,
            ai: section,
        }
    }

    fn vars() -> PathVars {
        let root = PathBuf::from(".");
        PathVars {
            app_data: root.clone(),
            app_config: root.clone(),
            app_cache: root.clone(),
            home: root.clone(),
            documents: root.clone(),
            downloads: root.clone(),
            desktop: root.clone(),
            temp: root,
            app: None,
        }
    }

    fn set(section: Option<AiPermissions>) -> PermissionSet {
        PermissionSet::from_manifest(&policy(section), &vars()).expect("valid policy")
    }

    #[test]
    fn specific_scope_requires_a_concrete_base_before_wildcards() {
        for scope in ["/home/alef-ai/**", "C:/Users/Example/**"] {
            assert!(specific_scope(scope), "{scope}");
        }
        for scope in ["/home/**", "C:/Users/**", "$HOME/**", "$HOME/*/later/**"] {
            assert!(!specific_scope(scope), "{scope}");
        }
    }

    #[test]
    fn builtin_names_scopes_and_defaults_are_validated() {
        let parsed: AiPermissions = serde_json::from_value(json!({"roles": [], "tools": [], "builtin": [{"name": "fs.read", "scopes": ["$HOME/Documents/*"]}, {"name": "exec"}]})).expect("builtin shape");
        validate_ai(&parsed).expect("supported builtins");
        let omitted: AiPermissions = serde_json::from_value(json!({"roles": [], "tools": []}))
            .expect("omitted builtin defaults");
        assert!(omitted.builtin.is_empty());
        for value in [
            json!({"roles": [], "tools": [], "builtin": [{"name": "unknown"}]}),
            json!({"roles": [], "tools": [], "builtin": [{"name": "exec", "scopes": ["x"]}]}),
            json!({"roles": [], "tools": [], "builtin": [{"name": "fs.read", "scopes": ["$HOME/../x"]}]}),
            json!({"roles": [], "tools": [], "builtin": [{"name": "fs.read", "scopes": ["/home/**"]}]}),
        ] {
            let parsed: AiPermissions = serde_json::from_value(value).expect("deserializes");
            error(&parsed);
        }
    }

    #[test]
    fn builtin_scope_and_name_duplicates_are_rejected() {
        for value in [
            json!({"roles": [], "tools": [], "builtin": [{"name": "fs.read", "scopes": ["$HOME/a", "$HOME/a"]}]}),
            json!({"roles": [], "tools": [], "builtin": [{"name": "exec"}, {"name": "exec"}]}),
        ] {
            let parsed: AiPermissions = serde_json::from_value(value).expect("deserializes");
            error(&parsed);
        }
    }

    #[test]
    fn an_absent_ai_section_is_closed_by_default() {
        let manifest = Manifest::from_ktav_str(MINIMAL).expect("fixture");
        assert_eq!(manifest.permissions.ai, None);
        let set = set(None);
        assert!(set.ai_roles().is_empty());
        assert_eq!(set.ai_tool("summarizeSelection"), None);
        assert!(set.rights().is_empty());
        for (permission, target) in [
            (Permission::AiRole, None),
            (Permission::AiRole, Some("smart")),
            (Permission::AiTool, Some("summarizeSelection")),
        ] {
            let err = set
                .check(permission, target, &Grants::new())
                .expect_err("nothing declared, nothing allowed");
            assert_eq!(err.code, ErrorCode::PermissionDenied);
            assert_eq!(err.details, Some(json!({"permission": permission.name()})));
        }
    }

    #[test]
    fn a_manifest_declares_roles_and_tools_and_their_rights() {
        let mut manifest = Manifest::from_ktav_str(MINIMAL)
            .expect("fixture")
            .permissions;
        manifest.ai = Some(valid());
        let set = set(manifest.ai.clone());
        assert_eq!(set.ai_roles(), &["smart"][..]);
        assert_eq!(
            set.ai_tool("summarizeSelection").map(|t| t.name.as_str()),
            Some("summarizeSelection")
        );
        assert_eq!(set.ai_tool("other"), None);
        for right in [
            Right::scoped("ai.role", "smart"),
            Right::scoped("ai.tool", "summarizeSelection"),
        ] {
            assert!(
                set.rights().contains(&right),
                "the declared {right} must be asked for"
            );
        }
    }

    #[test]
    fn a_tool_name_must_match_the_name_pattern_and_be_unique() {
        for name in [
            "_hidden",
            "1st",
            "",
            "with space",
            "a".repeat(65).as_str(),
            "héllo",
        ] {
            let err = error(&ai(&["smart"], vec![tool(name, json!({"type": "string"}))]));
            assert_eq!(
                err.details,
                Some(json!({"path": "ai.tools[0].name"})),
                "{name}"
            );
        }
        for role in ["_smart", "9lives", "a".repeat(65).as_str()] {
            let err = error(&ai(&[role], vec![simple_tool("t")]));
            assert_eq!(err.details, Some(json!({"path": "ai.roles[0]"})), "{role}");
        }
        let err = error(&ai(
            &["smart", "smart"],
            vec![simple_tool("a"), simple_tool("a")],
        ));
        assert_eq!(err.details, Some(json!({"path": "ai.roles[1]"})));
        let err = error(&ai(&["smart"], vec![simple_tool("t"), simple_tool("t")]));
        assert_eq!(err.details, Some(json!({"path": "ai.tools[1].name"})));
        // The two namespaces are separate: the same string may be a role and a tool.
        assert!(validate_ai(&ai(&["t"], vec![simple_tool("t")])).is_ok());
        // An empty description and an overlong one are rejected.
        let mut long = simple_tool("t");
        long.description = " ".repeat(513);
        let err = error(&ai(&["smart"], vec![long]));
        assert_eq!(
            err.details,
            Some(json!({"path": "ai.tools[0].description"}))
        );
    }

    #[test]
    fn a_schema_outside_the_subset_is_rejected() {
        let rejects: [(Value, &str); 16] = [
            (
                json!({"type": "object", "unknown": 1}),
                "ai.tools[0].schema.unknown",
            ),
            (
                json!({"type": "object", "$ref": "#/x"}),
                "ai.tools[0].schema.$ref",
            ),
            (json!({"type": "map"}), "ai.tools[0].schema.type"),
            (json!("string"), "ai.tools[0].schema"),
            (
                json!({
                    "type": "object",
                    "properties": {
                        "a": {
                            "type": "object",
                            "properties": {
                                "b": {
                                    "type": "object",
                                    "required": ["missing"],
                                    "additionalProperties": false
                                }
                            },
                            "additionalProperties": false
                        }
                    },
                    "additionalProperties": false
                }),
                "ai.tools[0].schema.properties.a.properties.b.required[0]",
            ),
            (
                json!({"type": "object", "required": ["missing"], "additionalProperties": false}),
                "ai.tools[0].schema.required[0]",
            ),
            (
                json!({"type": "string", "enum": []}),
                "ai.tools[0].schema.enum",
            ),
            (
                json!({"type": "string", "minLength": 4, "maxLength": 2}),
                "ai.tools[0].schema",
            ),
            (
                json!({"type": "number", "minimum": 5, "maximum": 1}),
                "ai.tools[0].schema",
            ),
            (
                json!({"type": "string", "maxLength": 65537}),
                "ai.tools[0].schema.maxLength",
            ),
            (
                json!({"type": "array", "maxItems": 4097}),
                "ai.tools[0].schema.maxItems",
            ),
            (
                json!({"type": "array", "maxItems": 10, "minItems": 11}),
                "ai.tools[0].schema",
            ),
            (
                json!({"type": "object", "properties": {}}),
                "ai.tools[0].schema.additionalProperties",
            ),
            (json!({"properties": {}}), "ai.tools[0].schema"),
            (
                json!({
                    "type": "object",
                    "properties": {"a": {"type": "string"}},
                    "required": ["a", "a"],
                    "additionalProperties": false
                }),
                "ai.tools[0].schema.required[1]",
            ),
            (
                json!({"type": "number", "minimum": "0"}),
                "ai.tools[0].schema.minimum",
            ),
        ];
        for (schema, path) in rejects {
            let err = error(&ai(&["smart"], vec![tool("t", schema)]));
            assert_eq!(err.details, Some(json!({"path": path})), "{err:?}");
        }
        // A deeper nesting reports the path of the violation, not of the root.
        let deep = wrapped(
            2,
            json!({"type": "object", "properties": {}, "additionalProperties": true}),
        );
        let err = error(&ai(&["smart"], vec![tool("t", deep)]));
        assert_eq!(
            err.details,
            Some(
                json!({"path": "ai.tools[0].schema.properties.a.properties.a.additionalProperties"})
            )
        );
        // 257 properties.
        let mut properties = serde_json::Map::new();
        for index in 0..257 {
            properties.insert(format!("p{index}"), json!({"type": "string"}));
        }
        let err = error(&ai(
            &["smart"],
            vec![tool(
                "t",
                json!(
                    {"type": "object", "properties": properties, "additionalProperties": false}
                ),
            )],
        ));
        assert_eq!(
            err.details,
            Some(json!({"path": "ai.tools[0].schema.properties"}))
        );
        // An array without a declared maximum and a 65-value enum.
        for schema in [
            json!({"type": "array", "items": {"type": "string"}}),
            json!({"type": "string", "enum": (0..65).map(Value::from).collect::<Vec<_>>()}),
        ] {
            let err = error(&ai(&["smart"], vec![tool("t", schema)]));
            assert!(err.details.is_some(), "{err:?}");
        }
        // Depth 33 is one level too deep; the path names the level of the violation.
        let err = error(&ai(&["smart"], vec![tool("t", nested(33))]));
        assert_eq!(
            err.details,
            Some(json!({
                "path": format!("ai.tools[0].schema{}", ".properties.a".repeat(33))
            }))
        );
    }

    #[test]
    fn a_schema_at_the_limits_is_accepted() {
        // Depth 32, 256 properties, an array at its maximum, a full-size schema.
        let mut properties = serde_json::Map::new();
        for index in 0..256 {
            properties.insert(format!("p{index}"), json!({"type": "string"}));
        }
        for schema in [
            nested(32),
            json!({
                "type": "object",
                "properties": properties,
                "additionalProperties": false
            }),
            json!({"type": "array", "items": {"type": "string"}, "maxItems": 4096, "minItems": 0}),
            json!({"type": "string", "minLength": 0, "maxLength": 65536, "enum": ["a", "b"]}),
        ] {
            assert!(
                validate_ai(&ai(&["smart"], vec![tool("t", schema)])).is_ok(),
                "the schema at the limit must be accepted"
            );
        }
        // A full-size schema that stays inside the subset: the growth adds typed objects of at
        // most 256 typed properties instead of untyped enums, and stays shallow.
        let mut wide = json!({"type": "string"});
        while serde_json::to_vec(&wide).unwrap().len() <= MAX_SCHEMA_BYTES {
            let mut properties = serde_json::Map::new();
            for index in 0..256 {
                properties.insert(format!("p{index}"), wide.clone());
                let grown = json!({
                    "type": "object",
                    "properties": properties.clone(),
                    "additionalProperties": false
                });
                if serde_json::to_vec(&grown).unwrap().len() > MAX_SCHEMA_BYTES {
                    break;
                }
            }
            wide = json!({
                "type": "object",
                "properties": properties,
                "additionalProperties": false
            });
        }
        let err = error(&ai(&["smart"], vec![tool("t", wide)]));
        assert_eq!(err.details, Some(json!({"path": "ai.tools[0].schema"})));
    }

    #[test]
    fn ai_rights_are_scoped_per_role_and_per_tool() {
        let set = set(Some(ai(
            &["smart", "fast"],
            vec![simple_tool("summarizeSelection")],
        )));
        let rights = set.rights();
        for right in [
            Right::scoped("ai.role", "smart"),
            Right::scoped("ai.role", "fast"),
            Right::scoped("ai.tool", "summarizeSelection"),
        ] {
            assert!(rights.contains(&right), "missing {right}");
        }
        assert_eq!(
            set.check(Permission::AiRole, Some("smart"), &Grants::new()),
            Ok(Decision::Allow)
        );
        let mut consent = Consent::undecided();
        consent.set(Right::scoped("ai.role", "smart"), Decision::Deny);
        let denied = set.with_consent(consent);
        let err = denied
            .check(Permission::AiRole, Some("smart"), &Grants::new())
            .expect_err("a denied role is refused");
        assert_eq!(err.code, ErrorCode::PermissionDenied);
        assert_eq!(err.details, Some(json!({"permission": "ai.role"})));
        // A role the manifest does not list is refused whatever the consent says.
        let err = denied
            .check(Permission::AiRole, Some("unknown"), &Grants::new())
            .expect_err("an undeclared role is refused");
        assert_eq!(err.details, Some(json!({"permission": "ai.role"})));
    }
}
