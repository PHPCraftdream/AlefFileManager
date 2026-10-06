// SPDX-License-Identifier: MIT OR Apache-2.0
//! Application manifest parsing and validation.

use serde::{Deserialize, Serialize};
use serde_json::json;

use super::window::WindowDef;
use crate::{AlefError, ErrorCode};

/// External-resource policy; all fields are required.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(export, export_to = "manifest.ts")]
pub struct External {
    /// Allowed connections.
    pub connect: Vec<String>,
    /// Resource loading policy.
    pub load: ExternalLoad,
}

/// External resource lists; all fields are required.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(export, export_to = "manifest.ts")]
pub struct ExternalLoad {
    /// Allowed scripts.
    pub scripts: Vec<String>,
    /// Allowed styles.
    pub styles: Vec<String>,
    /// Allowed images.
    pub images: Vec<String>,
    /// Allowed fonts.
    pub fonts: Vec<String>,
    /// Allowed media.
    pub media: Vec<String>,
    /// Allowed frames.
    pub frames: Vec<String>,
}

/// Filesystem permissions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(export, export_to = "manifest.ts")]
pub struct FsPermissions {
    /// Readable filesystem paths.
    pub read: Vec<String>,
    /// Writable filesystem paths.
    pub write: Vec<String>,
}

/// Command execution permissions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(export, export_to = "manifest.ts")]
pub struct CliPermissions {
    /// Executable commands.
    pub exec: Vec<String>,
}

/// Network permissions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(export, export_to = "manifest.ts")]
pub struct NetPermissions {
    /// HTTP destinations.
    pub http: Vec<String>,
    /// Socket destinations.
    pub socket: Vec<String>,
}

/// External shell permissions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(export, export_to = "manifest.ts")]
pub struct ShellPermissions {
    /// Allowed external URLs.
    #[serde(rename = "openExternal")]
    pub open_external: Vec<String>,
}

/// Clipboard permissions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(export, export_to = "manifest.ts")]
pub struct ClipboardPermissions {
    /// Whether clipboard reads are allowed.
    pub read: bool,
}

/// Global shortcut permissions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(export, export_to = "manifest.ts")]
pub struct ShortcutPermissions {
    /// Whether global shortcuts are allowed.
    pub global: bool,
}

/// Application environment permissions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(export, export_to = "manifest.ts")]
pub struct AppPermissions {
    /// Exposed environment variables.
    pub env: Vec<String>,
}

/// Window permissions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(export, export_to = "manifest.ts")]
pub struct WindowPermissions {
    /// Whether the application may open windows at runtime (`window.create`).
    pub create: bool,
}

/// Complete permission policy. Every subsection is required except `window`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(export, export_to = "manifest.ts")]
pub struct Permissions {
    /// Filesystem access.
    pub fs: FsPermissions,
    /// Command execution.
    pub cli: CliPermissions,
    /// Network access.
    pub net: NetPermissions,
    /// External shell access.
    pub shell: ShellPermissions,
    /// Clipboard access.
    pub clipboard: ClipboardPermissions,
    /// Global shortcuts.
    pub shortcut: ShortcutPermissions,
    /// Secret access.
    pub secrets: bool,
    /// Application environment access.
    pub app: AppPermissions,
    /// Runtime window creation; denied when the section is absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub window: Option<WindowPermissions>,
}

/// Value type of a command-line option.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "lowercase")]
#[ts(export, export_to = "manifest.ts")]
pub enum ArgKind {
    /// Any text.
    String,
    /// A finite number.
    Number,
    /// A flag: `--name` or `--name=true|false`.
    Boolean,
}

/// A declared option: `--name <value>`, `--name=<value>` or `-s <value>`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(export, export_to = "manifest.ts")]
pub struct ArgOption {
    /// Long name: lowercase letters, digits and hyphens; `help` and `version` are generated.
    pub name: String,
    /// One-character short name.
    #[ts(optional)]
    pub short: Option<String>,
    /// Value type.
    #[serde(rename = "type")]
    #[ts(rename = "type")]
    pub kind: ArgKind,
    /// Text of the generated `--help`.
    #[ts(optional)]
    pub description: Option<String>,
}

/// Where the arguments that are not options go.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(export, export_to = "manifest.ts")]
pub struct ArgPositional {
    /// Placeholder in the generated usage line.
    pub name: String,
    /// Text of the generated `--help`.
    #[ts(optional)]
    pub description: Option<String>,
}

/// Command-line schema of the application (`app.args()`, generated `--help`/`--version`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(export, export_to = "manifest.ts")]
pub struct Arguments {
    /// Declared options.
    pub options: Vec<ArgOption>,
    /// Positional arguments; without it a stray argument is a usage error.
    #[ts(optional)]
    pub positional: Option<ArgPositional>,
}

/// Application manifest with required security policy sections.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(export, export_to = "manifest.ts")]
pub struct Manifest {
    /// Reverse-DNS-like application identifier.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Non-empty version string.
    pub version: String,
    /// Declared windows; an empty list is allowed.
    pub windows: Vec<WindowDef>,
    /// External-resource policy.
    pub external: External,
    /// Permissions policy.
    pub permissions: Permissions,
    /// Command-line schema; an application without it takes no arguments.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub arguments: Option<Arguments>,
}

impl Manifest {
    /// Parse and validate an `alef.ktav` document; structured columns are zero-based byte columns.
    pub fn from_ktav_str(src: &str) -> Result<Self, AlefError> {
        let manifest: Self = ktav::from_str(src).map_err(|error| {
            let text = error.to_string();
            if let (Some(line), Some(span)) = (error.line(), error.span()) {
                let (_, column) = span.line_col(src);
                AlefError::new(
                    ErrorCode::ManifestInvalid,
                    format!("{text} (line {line}, column {column})"),
                )
                .with_details(json!({"line": line, "column": column}))
            } else if let Some(start) = text.find('`') {
                if let Some(end) = text[start + 1..].find('`') {
                    AlefError::new(ErrorCode::ManifestInvalid, text.clone())
                        .with_details(json!({"path": &text[start + 1..start + 1 + end]}))
                } else {
                    AlefError::new(ErrorCode::ManifestInvalid, text)
                }
            } else {
                AlefError::new(ErrorCode::ManifestInvalid, text)
            }
        })?;
        let invalid = |path: &str, reason: &str| {
            AlefError::new(ErrorCode::ManifestInvalid, format!("{path}: {reason}"))
                .with_details(json!({"path": path}))
        };
        if manifest.id.is_empty()
            || !manifest
                .id
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
            || manifest.id.split('.').any(str::is_empty)
        {
            return Err(invalid("id", "invalid reverse-DNS-like identifier"));
        }
        if manifest.version.trim().is_empty() {
            return Err(invalid("version", "must not be empty"));
        }
        for (index, window) in manifest.windows.iter().enumerate() {
            window
                .check()
                .map_err(|(field, reason)| invalid(&format!("windows[{index}].{field}"), reason))?;
        }
        for index in 0..manifest.windows.len() {
            if manifest.windows[..index]
                .iter()
                .any(|window| window.label == manifest.windows[index].label)
            {
                return Err(invalid(
                    &format!("windows[{index}].label"),
                    "duplicate label",
                ));
            }
        }
        if let Some(arguments) = &manifest.arguments {
            validate_arguments(arguments).map_err(|(path, reason)| invalid(&path, reason))?;
        }
        Ok(manifest)
    }
}

/// Generated by the runtime: declaring them would shadow `--help`/`--version`.
const RESERVED_LONG: [&str; 2] = ["help", "version"];
const RESERVED_SHORT: [char; 2] = ['h', 'V'];

/// Lowercase letters, digits and single hyphens inside; starts with a letter.
fn kebab(name: &str) -> bool {
    let mut chars = name.chars();
    chars.next().is_some_and(|c| c.is_ascii_lowercase())
        && name
            .chars()
            .all(|c| c.is_ascii_lowercase() || c.is_ascii_digit() || c == '-')
        && !name.ends_with('-')
        && !name.contains("--")
}

fn validate_arguments(arguments: &Arguments) -> Result<(), (String, &'static str)> {
    for (index, option) in arguments.options.iter().enumerate() {
        let path = format!("arguments.options[{index}]");
        if !kebab(&option.name) {
            return Err((
                format!("{path}.name"),
                "must be lowercase letters, digits and hyphens, starting with a letter",
            ));
        }
        if RESERVED_LONG.contains(&option.name.as_str()) {
            return Err((format!("{path}.name"), "help and version are generated"));
        }
        if arguments.options[..index]
            .iter()
            .any(|other| other.name == option.name)
        {
            return Err((format!("{path}.name"), "duplicate option"));
        }
        if let Some(short) = &option.short {
            let mut chars = short.chars();
            let single = match (chars.next(), chars.next()) {
                (Some(c), None) if c.is_ascii_alphanumeric() => Some(c),
                _ => None,
            };
            let Some(short) = single else {
                return Err((format!("{path}.short"), "must be one ASCII letter or digit"));
            };
            if RESERVED_SHORT.contains(&short) {
                return Err((format!("{path}.short"), "-h and -V are generated"));
            }
            if arguments.options[..index]
                .iter()
                .any(|other| other.short.as_deref() == Some(&short.to_string()))
            {
                return Err((format!("{path}.short"), "duplicate short option"));
            }
        }
    }
    if let Some(positional) = &arguments.positional {
        if !kebab(&positional.name) {
            return Err((
                "arguments.positional.name".to_owned(),
                "must be lowercase letters, digits and hyphens, starting with a letter",
            ));
        }
    }
    Ok(())
}
