// SPDX-License-Identifier: MIT OR Apache-2.0
//! Application manifest parsing and validation.

use serde::{Deserialize, Serialize};
use serde_json::json;

use super::window::WindowDef;
use crate::{AlefError, ErrorCode};

mod ai;
pub use ai::{validate_ai, AiBuiltin, AiPermissions, AiTool};
mod assets;
use assets::{deserialize_assets, field_line};
mod runtime;
pub use runtime::{deserialize_runtime, runtime_requirement_met, validate_package_version};
mod updater;
use updater::{deserialize_updater, validate_updater};

/// Compare strict SemVer versions by precedence, ignoring build metadata.
pub fn compare_versions(left: &str, right: &str) -> Result<std::cmp::Ordering, semver::Error> {
    runtime::compare_versions(left, right)
}

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
    /// Executable commands; omitted means no direct execution permission.
    #[serde(default)]
    #[ts(optional, as = "Option<Vec<String>>")]
    pub exec: Vec<String>,
    /// Fixed commands the page may run by name; each is a right of its own (`cli.command`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(optional, as = "Option<Vec<CliCommand>>")]
    pub commands: Vec<CliCommand>,
}

/// A declared command: the program and the argument template are fixed by the manifest.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(export, export_to = "manifest.ts")]
pub struct CliCommand {
    /// Lowercase letters, digits and hyphens, not starting with a hyphen; unique in the list.
    pub name: String,
    /// A bare program name, an absolute path or `sidecar:<name>`.
    pub program: String,
    /// Arguments; an element that is exactly `{param}` is replaced by the value the page passes
    /// (`param`: a lowercase letter, then letters and digits), any other element is passed as is.
    /// In Ktav write brace-enclosed elements as raw items (`:: {path}`).
    pub args: Vec<String>,
    /// Non-empty text shown to the user.
    pub description: String,
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
    /// Whether the page may ask for the system context menu of files; denied when omitted.
    #[serde(default, rename = "contextMenu")]
    #[ts(optional, as = "Option<bool>")]
    pub context_menu: bool,
    /// Whether an elevating (`runas`) item may be invoked; refused when omitted.
    #[serde(default, rename = "contextMenuRunas")]
    #[ts(optional, as = "Option<bool>")]
    pub context_menu_runas: bool,
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
    /// Whether the application may manage autostart.
    #[serde(default)]
    #[ts(optional, as = "Option<bool>")]
    pub autostart: bool,
}

/// Window permissions.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(export, export_to = "manifest.ts")]
pub struct WindowPermissions {
    /// Whether the application may open windows at runtime (`window.create`).
    pub create: bool,
}

/// Complete permission policy. Every subsection is required except `window`; `tray` defaults to false.
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
    /// Whether the application may manage a tray icon; denied when omitted.
    #[serde(default)]
    #[ts(optional, as = "Option<bool>")]
    pub tray: bool,
    /// Secret access.
    pub secrets: bool,
    /// Whether the page gets the real WebCrypto (`crypto.subtle`); substituted when omitted.
    #[serde(default)]
    #[ts(optional, as = "Option<bool>")]
    pub webcrypto: bool,
    /// Application environment access.
    pub app: AppPermissions,
    /// Runtime window creation; denied when the section is absent.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub window: Option<WindowPermissions>,
    /// AI roles and application tools (M9); closed by default: denied when the section is absent.
    #[serde(default, rename = "ai", skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub ai: Option<AiPermissions>,
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

/// Launch modes selectable by application code (M5 integration manifest rules).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "lowercase")]
#[ts(export, export_to = "manifest.ts")]
pub enum Mode {
    /// Console utility with standard streams.
    Cli,
    /// Long-lived foreground process without windows.
    Foreground,
    /// Declared application windows.
    Window,
}

/// Updater configuration.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, ts_rs::TS)]
#[serde(deny_unknown_fields)]
#[ts(export, export_to = "manifest.ts")]
pub struct Updater {
    /// HTTPS feed endpoint, or HTTP on a loopback host.
    pub endpoint: String,
    /// Canonical RFC 4648 base64 encoding of a 32-byte Ed25519 public key.
    #[serde(rename = "publicKey")]
    #[ts(rename = "publicKey")]
    pub public_key: String,
}

/// Application manifest.
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
    /// Optional runtime requirement using exactly `semver::VersionReq` grammar.
    /// Absence makes no compatibility promise; explicit null is invalid.
    #[serde(
        default,
        deserialize_with = "deserialize_runtime",
        skip_serializing_if = "Option::is_none"
    )]
    #[ts(optional)]
    pub runtime: Option<String>,
    /// Public proper subdirectory; absence leaves `dist` selection to the asset source.
    // Explicit null is not absence: a present field must be a valid String.
    #[serde(
        default,
        deserialize_with = "deserialize_assets",
        skip_serializing_if = "Option::is_none"
    )]
    #[ts(optional)]
    pub assets: Option<String>,
    /// Declared windows; an empty list is allowed (a console utility or a service).
    pub windows: Vec<WindowDef>,
    /// A console utility: no window, and the process has stdin, stdout and stderr (`app.stdin`...).
    /// Needs `windows: []`.
    #[serde(default)]
    pub console: bool,
    /// Selectable launch modes; empty or omitted preserves legacy startup.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    #[ts(optional, as = "Option<Vec<Mode>>")]
    pub modes: Vec<Mode>,
    /// Entry document for headless startup or mode selection (root-relative, `/` when omitted).
    /// With windows, requires nonempty `modes`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub entry: Option<String>,
    /// External-resource policy.
    pub external: External,
    /// Permissions policy.
    pub permissions: Permissions,
    /// Command-line schema; an application without it takes no arguments.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    #[ts(optional)]
    pub arguments: Option<Arguments>,
    /// Declared deep-link schemes; at most eight unique lowercase schemes of 1–64 bytes.
    #[serde(default, rename = "deepLinks", skip_serializing_if = "Vec::is_empty")]
    #[ts(optional, as = "Option<Vec<String>>")]
    pub deep_links: Vec<String>,
    /// Optional signed-update feed configuration.
    #[serde(
        default,
        deserialize_with = "deserialize_updater",
        skip_serializing_if = "Option::is_none"
    )]
    #[ts(optional)]
    pub updater: Option<Updater>,
}

impl Manifest {
    /// Parse and validate an `alef.ktav` document; structured columns are zero-based byte columns.
    pub fn from_ktav_str(src: &str) -> Result<Self, AlefError> {
        let manifest: Self = ktav::from_str(src).map_err(|error| {
            let text = error.to_string();
            if let Some(path) = ["assets", "runtime"]
                .into_iter()
                .find(|path| text.starts_with(&format!("{path}:")))
            {
                let line = field_line(src, path);
                return AlefError::new(ErrorCode::ManifestInvalid, format!("{text} (line {line})"))
                    .with_details(json!({"path": path, "line": line}));
            }
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
        // docs/stages/m5-integration.md: nonempty modes opt into launch selection.
        if !manifest.modes.is_empty() {
            if manifest.console {
                return Err(invalid("console", "incompatible with nonempty modes"));
            }
            if manifest.modes.contains(&Mode::Window) == manifest.windows.is_empty() {
                return Err(invalid(
                    "modes",
                    "window mode requires windows and vice versa",
                ));
            }
            for (index, mode) in manifest.modes.iter().enumerate() {
                if manifest.modes[..index].contains(mode) {
                    return Err(invalid(&format!("modes[{index}]"), "duplicate mode"));
                }
            }
        }
        if !manifest.windows.is_empty() {
            if manifest.console {
                return Err(invalid("console", "needs windows: []"));
            }
            if manifest.entry.is_some() && manifest.modes.is_empty() {
                return Err(invalid("entry", "needs windows: []"));
            }
        }
        if let Some(entry) = &manifest.entry {
            if !entry.starts_with('/') {
                return Err(invalid("entry", "must start with /"));
            }
            if entry.starts_with("//") || entry.contains('\\') {
                return Err(invalid(
                    "entry",
                    "must be a path of the application, not another host",
                ));
            }
        }
        validate_deep_links(&manifest.deep_links)?;
        if let Some(updater) = &manifest.updater {
            validate_updater(updater, &manifest.permissions.net.http)?;
        }
        super::command::parse_cli(&manifest.permissions.cli)?;
        if let Some(ai) = &manifest.permissions.ai {
            validate_ai(ai)?;
        }
        if let Some(arguments) = &manifest.arguments {
            validate_arguments(arguments).map_err(|(path, reason)| invalid(&path, reason))?;
        }
        Ok(manifest)
    }
}

/// Shared validation for manifest loading and runtime permission builders.
pub(super) fn validate_deep_links(schemes: &[String]) -> Result<(), AlefError> {
    let invalid = |path: &str, reason: &str| {
        AlefError::new(ErrorCode::ManifestInvalid, format!("{path}: {reason}"))
            .with_details(json!({"path": path}))
    };
    if schemes.len() > 8 {
        return Err(invalid("deepLinks", "must contain at most eight schemes"));
    }
    for (index, scheme) in schemes.iter().enumerate() {
        let path = format!("deepLinks[{index}]");
        if scheme.len() > 64
            || !scheme
                .bytes()
                .next()
                .is_some_and(|b| b.is_ascii_lowercase())
            || !scheme
                .bytes()
                .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit() || b"+-.".contains(&b))
        {
            return Err(invalid(
                &path,
                "must be a lowercase scheme of 1–64 ASCII bytes",
            ));
        }
        if [
            "http",
            "https",
            "file",
            "ftp",
            "ws",
            "wss",
            "data",
            "blob",
            "javascript",
            "about",
            "mailto",
            "tel",
        ]
        .contains(&scheme.as_str())
        {
            return Err(invalid(&path, "reserved scheme"));
        }
        if schemes[..index].contains(scheme) {
            return Err(invalid(&path, "duplicate scheme"));
        }
    }
    Ok(())
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

#[cfg(test)]
mod mode_tests {
    use super::{Manifest, Mode};
    use crate::ErrorCode;
    const MINIMAL: &str = include_str!("../../../tests/fixtures/minimal.ktav");
    fn error(src: &str) -> crate::AlefError {
        let err = Manifest::from_ktav_str(src).expect_err("manifest must be rejected");
        assert_eq!(err.code, ErrorCode::ManifestInvalid);
        err
    }
    fn without_windows(extra: &str) -> String {
        let mut s = MINIMAL.to_string();
        let start = s.find("windows: [").unwrap();
        let end = s[start..].find("]\nexternal").unwrap() + start + 1;
        s.replace_range(start..end, &format!("windows: []\n{extra}"));
        s
    }
    #[test]
    fn modes_accept_all_valid_subsets_and_round_trip_lowercase() {
        for mask in 1..8 {
            let modes: Vec<_> = [
                (Mode::Cli, "cli"),
                (Mode::Foreground, "foreground"),
                (Mode::Window, "window"),
            ]
            .into_iter()
            .enumerate()
            .filter(|(i, _)| mask & (1 << i) != 0)
            .map(|(_, mode)| mode)
            .collect();
            let items: String = modes
                .iter()
                .map(|(_, name)| format!("    :: {name}\n"))
                .collect();
            let extra = format!("modes: [\n{items}]\n");
            let source = if mask & 4 != 0 {
                format!("{MINIMAL}\n{extra}")
            } else {
                without_windows(&extra)
            };
            let manifest = Manifest::from_ktav_str(&source).unwrap();
            assert_eq!(
                manifest.modes,
                modes.iter().map(|(mode, _)| *mode).collect::<Vec<_>>()
            );
            assert_eq!(
                manifest.entry, None,
                "omitted entry retains the legacy representation"
            );
            let json = serde_json::to_value(&manifest).unwrap();
            assert_eq!(
                json["modes"],
                serde_json::json!(modes.iter().map(|(_, name)| *name).collect::<Vec<_>>())
            );
            assert_eq!(serde_json::from_value::<Manifest>(json).unwrap(), manifest);
        }
    }
    #[test]
    fn modes_reject_console_window_mismatches_duplicates_and_unknown_values() {
        for (source, path) in [
            (
                without_windows("modes: [\n    :: cli\n]\nconsole: true"),
                "console",
            ),
            (
                without_windows("modes: [\n    :: foreground\n]\nconsole: true"),
                "console",
            ),
            (
                format!("{MINIMAL}\nmodes: [\n    :: window\n]\nconsole: true\n"),
                "console",
            ),
            (without_windows("modes: [\n    :: window\n]"), "modes"),
            (format!("{MINIMAL}\nmodes: [\n    :: cli\n]\n"), "modes"),
            (
                format!("{MINIMAL}\nmodes: [\n    :: foreground\n]\n"),
                "modes",
            ),
            (
                without_windows("modes: [\n    :: cli\n    :: cli\n]"),
                "modes[1]",
            ),
            (
                without_windows("modes: [\n    :: foreground\n    :: cli\n    :: foreground\n]"),
                "modes[2]",
            ),
            (
                format!("{MINIMAL}\nmodes: [\n    :: window\n    :: window\n]\n"),
                "modes[1]",
            ),
        ] {
            assert_eq!(
                error(&source).details,
                Some(serde_json::json!({"path": path})),
                "{source}"
            );
        }
        for value in ["unknown", "Cli", "foregrounded", "undecided"] {
            let err = error(&without_windows(&format!("modes: [\n    :: {value}\n]")));
            assert!(err.message.contains("unknown variant"), "{err:?}");
        }
        for value in ["null", "cli", "1", "true", "{}"] {
            error(&without_windows(&format!("modes: {value}")));
        }
    }
    #[test]
    fn modes_entry_uses_existing_root_relative_policy_with_and_without_windows() {
        for source in [
            without_windows("modes: [\n    :: cli\n    :: foreground\n]"),
            format!("{MINIMAL}\nmodes: [\n    :: window\n]\n"),
        ] {
            for entry in ["/", "/choose.html", "/nested/choose.html?mode=cli#start"] {
                let manifest =
                    Manifest::from_ktav_str(&format!("{source}\nentry: {entry}\n")).unwrap();
                assert_eq!(manifest.entry.as_deref(), Some(entry));
            }
            for entry in [
                "choose.html",
                "https://evil.example/x",
                "//evil.example/x",
                "/a\\b",
            ] {
                let err = error(&format!("{source}\nentry: {entry}\n"));
                assert_eq!(err.details, Some(serde_json::json!({"path": "entry"})));
            }
        }
    }
    #[test]
    fn modes_empty_and_absent_preserve_legacy_defaults_and_rejections() {
        for source in [
            MINIMAL.to_owned(),
            without_windows(""),
            without_windows("console: true\nentry: /tool.html"),
            without_windows("console: false\nentry: /service.html"),
        ] {
            let legacy = Manifest::from_ktav_str(&source).unwrap();
            assert!(legacy.modes.is_empty());
            assert_eq!(
                Manifest::from_ktav_str(&format!("{source}\nmodes: []\n")).unwrap(),
                legacy
            );
            let mut json = serde_json::to_value(&legacy).unwrap();
            json.as_object_mut().unwrap().remove("modes");
            assert_eq!(serde_json::from_value::<Manifest>(json).unwrap(), legacy);
        }
        for extra in ["console: true", "entry: /choose.html"] {
            let err = error(&format!("{MINIMAL}\nmodes: []\n{extra}\n"));
            assert!(err.message.contains("needs windows: []"), "{err:?}");
        }
    }
}
