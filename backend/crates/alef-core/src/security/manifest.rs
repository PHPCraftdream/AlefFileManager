// SPDX-License-Identifier: MIT OR Apache-2.0
//! Application manifest parsing and validation.

use serde::{Deserialize, Serialize};
use serde_json::json;

use super::window::{Length, WindowDef};
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

/// Complete permission policy. Every subsection is required.
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
            let path = format!("windows[{index}]");
            if window.label.trim().is_empty() {
                return Err(invalid(&format!("{path}.label"), "must not be empty"));
            }
            if !window.url.starts_with('/') {
                return Err(invalid(&format!("{path}.url"), "must start with /"));
            }
            for (minimum, maximum, axis) in [
                (window.min_width, window.max_width, "width"),
                (window.min_height, window.max_height, "height"),
            ] {
                if let (Some(minimum), Some(maximum)) = (minimum, maximum) {
                    let exceeds = match (minimum, maximum) {
                        (Length::Px(a), Length::Px(b)) => Some(a > b),
                        (Length::Percent(a, unit_a), Length::Percent(b, unit_b))
                            if unit_a == unit_b =>
                        {
                            Some(a > b)
                        }
                        _ => None,
                    };
                    if exceeds == Some(true) {
                        return Err(invalid(&format!("{path}.min{axis}"), "exceeds maximum"));
                    }
                }
            }
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
        Ok(manifest)
    }
}
