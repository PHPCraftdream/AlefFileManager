// SPDX-License-Identifier: MIT OR Apache-2.0
//! What `alef` does for one application directory: the manifest read from `alef.ktav` and the
//! window, permissions and CSP derived from it. A manifest the runtime cannot honour is refused
//! up front with a clear error instead of being silently reduced.
use std::{
    path::{Path, PathBuf},
    sync::Arc,
};

use alef_core::{
    error::{AlefError, ErrorCode},
    security::{
        csp::build_csp,
        manifest::{Manifest, Mode},
        permissions::{PathVars, PermissionSet},
    },
};

pub const MANIFEST_FILE: &str = "alef.ktav";

/// Origin of the application documents; `build_csp` allows it explicitly (see its docs).
const CSP_APP_ORIGIN: &str = "native://app";

pub struct Plan {
    pub manifest: Manifest,
    pub permissions: Arc<PermissionSet>,
    pub csp: String,
    pub assets: PathBuf,
    initial_mode: Option<Mode>,
}

impl Plan {
    /// An application without windows (a console utility or a service) runs in a hidden WebView.
    pub fn windowless(&self) -> bool {
        self.initial_mode != Some(Mode::Window)
    }

    /// Applies the internal child mode before any engine is created.
    pub fn select_window_child(&mut self) -> Result<(), AlefError> {
        if !self.manifest.modes.contains(&Mode::Window) {
            return Err(AlefError::new(
                ErrorCode::InvalidArgument,
                "--launch-mode window requires manifest window mode",
            ));
        }
        self.initial_mode = Some(Mode::Window);
        Ok(())
    }

    pub fn initial_mode(&self) -> Option<Mode> {
        self.initial_mode
    }

    pub fn allowed_modes(&self) -> Vec<Mode> {
        if self.manifest.modes.is_empty() {
            vec![self.initial_mode.expect("legacy mode")]
        } else {
            self.manifest.modes.clone()
        }
    }

    /// Undecided processes load the decision document, children load their first window.
    pub fn entry(&self) -> String {
        if self.initial_mode == Some(Mode::Window) {
            self.manifest.windows[0].url.clone()
        } else {
            self.manifest
                .entry
                .clone()
                .unwrap_or_else(|| "/".to_owned())
        }
    }
}

/// Test-only override: decimal milliseconds in 1..=30000, ignored outside ALEF_E2E=1.
/// The argument form keeps timeout injection independent of process-wide environment mutation.
pub fn launch_timeout(
    e2e: Option<&std::ffi::OsStr>,
    value: Option<&std::ffi::OsStr>,
) -> Result<std::time::Duration, String> {
    let default = std::time::Duration::from_secs(30);
    if e2e != Some(std::ffi::OsStr::new("1")) {
        return Ok(default);
    }
    let Some(value) = value else {
        return Ok(default);
    };
    let invalid =
        || "ALEF_E2E_LAUNCH_TIMEOUT_MS must be decimal milliseconds in 1..=30000".to_owned();
    let text = value.to_str().ok_or_else(invalid)?;
    if text.is_empty() || !text.bytes().all(|b| b.is_ascii_digit()) {
        return Err(invalid());
    }
    let milliseconds = text.parse::<u64>().map_err(|_| invalid())?;
    if !(1..=30000).contains(&milliseconds) {
        return Err(invalid());
    }
    Ok(std::time::Duration::from_millis(milliseconds))
}

/// Reads and validates `<app_dir>/alef.ktav`.
pub fn load_manifest(app_dir: &Path) -> Result<Manifest, AlefError> {
    let path = app_dir.join(MANIFEST_FILE);
    let text = std::fs::read_to_string(&path).map_err(|error| {
        AlefError::new(
            ErrorCode::NotFound,
            format!("cannot read {MANIFEST_FILE} in the application directory: {error}"),
        )
    })?;
    Manifest::from_ktav_str(&text)
}

/// Derives the launch plan; `vars` are the directories behind the `$NAME` scope variables.
pub fn make_plan(app_dir: &Path, manifest: Manifest, vars: &PathVars) -> Result<Plan, AlefError> {
    let permissions = Arc::new(
        PermissionSet::from_manifest(&manifest.permissions, vars)?
            .with_deep_links(&manifest.deep_links)?,
    );
    let csp = build_csp(&manifest.external, CSP_APP_ORIGIN)?;
    Ok(Plan {
        permissions,
        csp,
        assets: app_dir.to_path_buf(),
        initial_mode: if !manifest.modes.is_empty() {
            None
        } else if !manifest.windows.is_empty() {
            Some(Mode::Window)
        } else if manifest.console {
            Some(Mode::Cli)
        } else {
            Some(Mode::Foreground)
        },
        manifest,
    })
}

/// The directories behind `$APPDATA`, `$HOME` and the other scope variables of application `id`.
pub fn path_vars(app_dir: &Path, id: &str) -> Result<PathVars, AlefError> {
    let plain = !id.is_empty()
        && !id.starts_with('.')
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '.' | '-' | '_'));
    if !plain {
        return Err(AlefError::new(
            ErrorCode::ManifestInvalid,
            "id: must be a plain identifier (letters, digits, '.', '-', '_')",
        ));
    }
    let app = std::fs::canonicalize(app_dir).map_err(|error| {
        AlefError::new(
            ErrorCode::NotFound,
            format!("cannot open the application directory: {error}"),
        )
    })?;
    let home = dirs::home_dir().unwrap_or_else(|| app.clone());
    // Application-owned directories live under the manifest id, never directly in the base.
    let owned = |base: Option<PathBuf>, fallback: &str| {
        base.unwrap_or_else(|| home.join(fallback)).join(id)
    };
    Ok(PathVars {
        app_data: owned(dirs::data_local_dir(), ".local/share"),
        app_config: owned(dirs::config_dir(), ".config"),
        app_cache: owned(dirs::cache_dir(), ".cache"),
        documents: dirs::document_dir().unwrap_or_else(|| home.join("Documents")),
        downloads: dirs::download_dir().unwrap_or_else(|| home.join("Downloads")),
        desktop: dirs::desktop_dir().unwrap_or_else(|| home.join("Desktop")),
        temp: std::env::temp_dir(),
        app,
        home,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use alef_core::security::window::Monitor;

    fn manifest(window: &str, permissions_fs_read: &str) -> Manifest {
        let text = format!(
            "id: org.example.app
name: Example
version:: 1.0.0
windows: [
    {{
        label: main
        url: /index.html
{window}    }}
]
external: {{
    connect: []
    load: {{
        scripts: []
        styles: []
        images: []
        fonts: []
        media: []
        frames: []
    }}
}}
permissions: {{
    fs: {{
        read: {permissions_fs_read}
        write: []
    }}
    cli: {{
        exec: []
    }}
    net: {{
        http: []
        socket: []
    }}
    shell: {{
        openExternal: []
    }}
    clipboard: {{
        read: false
    }}
    shortcut: {{
        global: false
    }}
    secrets: false
    app: {{
        env: []
    }}
}}
"
        );
        Manifest::from_ktav_str(&text).expect("valid manifest")
    }

    const SIZE: &str = "        width: 800
        height: 600
";

    fn plan_of(manifest: Manifest) -> Result<Plan, AlefError> {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let vars = path_vars(dir, "org.example.app").expect("vars");
        make_plan(dir, manifest, &vars)
    }

    #[test]
    fn the_timeout_override_is_strict_bounded_and_only_enabled_for_e2e() {
        use std::{ffi::OsStr, time::Duration};
        assert_eq!(
            launch_timeout(None, Some(OsStr::new("bad"))).unwrap(),
            Duration::from_secs(30)
        );
        assert_eq!(
            launch_timeout(Some(OsStr::new("true")), Some(OsStr::new("1"))).unwrap(),
            Duration::from_secs(30)
        );
        assert_eq!(
            launch_timeout(Some(OsStr::new("1")), None).unwrap(),
            Duration::from_secs(30)
        );
        for value in ["1", "30000"] {
            assert_eq!(
                launch_timeout(Some(OsStr::new("1")), Some(OsStr::new(value))).unwrap(),
                Duration::from_millis(value.parse().unwrap())
            );
        }
        for bad in [
            "",
            "0",
            "30001",
            "-1",
            "+1",
            " 1",
            "1 ",
            "1.0",
            "1e2",
            "18446744073709551616",
        ] {
            assert!(
                launch_timeout(Some(OsStr::new("1")), Some(OsStr::new(bad))).is_err(),
                "{bad:?}"
            );
        }
    }

    #[test]
    fn mode_inference_preserves_legacy_launches_and_undecided_processes_use_the_entry_document() {
        let mut declaration = manifest(SIZE, "[]");
        let legacy = plan_of(declaration.clone()).unwrap();
        assert_eq!(legacy.initial_mode(), Some(Mode::Window));
        assert!(!legacy.windowless());
        assert_eq!(legacy.entry(), "/index.html");
        assert!(plan_of(declaration.clone())
            .unwrap()
            .select_window_child()
            .is_err());
        declaration.modes = vec![Mode::Cli, Mode::Foreground, Mode::Window];
        let plan = plan_of(declaration.clone()).unwrap();
        assert_eq!(plan.initial_mode(), None);
        assert!(plan.windowless());
        assert_eq!(plan.entry(), "/");
        declaration.entry = Some("/decision.html".into());
        let mut plan = plan_of(declaration.clone()).unwrap();
        assert_eq!(plan.entry(), "/decision.html");
        plan.select_window_child().unwrap();
        assert_eq!(plan.initial_mode(), Some(Mode::Window));
        assert!(!plan.windowless());
        assert_eq!(plan.entry(), "/index.html");
        declaration.windows.clear();
        declaration.modes.clear();
        for (console, mode) in [(true, Mode::Cli), (false, Mode::Foreground)] {
            declaration.console = console;
            let plan = plan_of(declaration.clone()).unwrap();
            assert_eq!(plan.initial_mode(), Some(mode));
            assert_eq!(plan.allowed_modes(), vec![mode]);
            assert!(plan.windowless());
        }
        declaration.modes = vec![Mode::Cli];
        assert!(plan_of(declaration).unwrap().select_window_child().is_err());
    }

    #[test]
    fn a_plain_manifest_becomes_a_permission_set_and_a_csp() {
        let mut declaration = manifest(SIZE, "[ $APP/data/* ]");
        declaration.deep_links = vec!["alef".into()];
        let plan = plan_of(declaration).expect("plan");
        assert!(plan
            .permissions
            .rights()
            .contains(&alef_core::security::consent::Right::scoped(
                "app.deepLinks",
                "alef"
            )));
        assert_eq!(plan.manifest.windows.len(), 1);
        assert_eq!(plan.manifest.windows[0].url, "/index.html");
        assert!(plan
            .csp
            .starts_with("default-src 'none'; script-src 'self' native://app;"));
        assert!(plan.csp.contains("connect-src 'self' native:"));
        assert_eq!(plan.manifest.id, "org.example.app");
    }

    #[test]
    fn tray_is_denied_when_omitted_and_listed_when_explicitly_enabled() {
        use alef_core::security::{
            consent::{Decision, Right},
            grants::Grants,
            permissions::Permission,
        };
        let plain = manifest(SIZE, "[]");
        assert!(!plain.permissions.tray);
        let plan = plan_of(plain.clone()).unwrap();
        assert!(!plan.permissions.rights().contains(&Right::plain("tray")));
        assert_eq!(
            plan.permissions
                .check(Permission::Tray, None, &Grants::new())
                .unwrap_err()
                .code,
            ErrorCode::PermissionDenied
        );
        let mut enabled = plain;
        enabled.permissions.tray = true;
        let plan = plan_of(enabled).unwrap();
        assert_eq!(plan.permissions.rights(), [Right::plain("tray")]);
        assert_eq!(
            plan.permissions
                .check(Permission::Tray, None, &Grants::new())
                .unwrap(),
            Decision::Allow
        );
    }

    #[test]
    fn percentages_limits_monitor_and_position_are_the_window_module_s_to_resolve() {
        let plan = plan_of(manifest(
            "        width: 70%work
        height: 50%screen
        minWidth: 400
        maxHeight: 90%work
        monitor: cursor
        position: { x: 10, y: 20 }
",
            "[]",
        ))
        .expect("the runtime resolves these when it opens the window");
        let window = &plan.manifest.windows[0];
        assert_eq!(window.monitor, Monitor::Cursor);
        assert!(window.max_height.is_some());
    }

    #[test]
    fn several_windows_are_all_kept() {
        let mut two = manifest(SIZE, "[]");
        let mut second = two.windows[0].clone();
        second.label = "tool".to_owned();
        two.windows.push(second);
        let plan = plan_of(two).expect("plan");
        let labels: Vec<_> = plan
            .manifest
            .windows
            .iter()
            .map(|w| w.label.as_str())
            .collect();
        assert_eq!(labels, ["main", "tool"]);
    }

    #[test]
    fn a_window_that_asks_to_be_restored_is_accepted_and_keeps_its_flag() {
        let plan = plan_of(manifest(
            &format!(
                "{SIZE}        restore: true
"
            ),
            "[]",
        ))
        .expect("the runtime remembers windows now");
        assert!(plan.manifest.windows[0].restore);
        let plain = plan_of(manifest(SIZE, "[]")).expect("plan");
        assert!(
            !plain.manifest.windows[0].restore,
            "restore is off unless asked for"
        );
    }

    #[test]
    fn an_application_without_windows_is_windowless_and_its_entry_is_its_own_or_the_root() {
        let windowed = plan_of(manifest(SIZE, "[]")).expect("plan");
        assert!(!windowed.windowless());
        assert_eq!(
            windowed.entry(),
            "/index.html",
            "the url of the first window"
        );
        let mut none = manifest(SIZE, "[]");
        none.windows.clear();
        let service = plan_of(none.clone()).expect("a service has no window");
        assert!(service.windowless());
        assert_eq!(service.entry(), "/");
        none.entry = Some("/tool.html".to_owned());
        assert_eq!(plan_of(none).expect("plan").entry(), "/tool.html");
    }

    #[test]
    fn a_missing_manifest_is_reported_and_a_broken_one_names_the_field() {
        let dir = tempfile::tempdir().expect("dir");
        assert_eq!(
            load_manifest(dir.path()).expect_err("missing").code,
            ErrorCode::NotFound
        );
        std::fs::write(
            dir.path().join(MANIFEST_FILE),
            "id: x
",
        )
        .expect("write");
        let error = load_manifest(dir.path()).expect_err("incomplete");
        assert_eq!(error.code, ErrorCode::ManifestInvalid);
        assert!(
            error.message.contains("name") || error.message.contains("missing"),
            "{}",
            error.message
        );
    }

    #[test]
    fn path_variables_are_absolute_and_application_directories_use_the_id() {
        let dir = Path::new(env!("CARGO_MANIFEST_DIR"));
        let vars = path_vars(dir, "org.example.app").expect("vars");
        for path in [
            &vars.app,
            &vars.home,
            &vars.temp,
            &vars.app_data,
            &vars.app_config,
            &vars.app_cache,
        ] {
            assert!(path.is_absolute(), "{path:?}");
        }
        for owned in [&vars.app_data, &vars.app_config, &vars.app_cache] {
            assert!(owned.ends_with("org.example.app"), "{owned:?}");
        }
        for bad in ["", ".hidden", "a/b", r"a\b", "..", "a b", "x:y"] {
            assert_eq!(
                path_vars(dir, bad).expect_err("bad id").code,
                ErrorCode::ManifestInvalid,
                "{bad:?}"
            );
        }
        assert_eq!(
            path_vars(&dir.join("does-not-exist"), "org.example.app")
                .expect_err("missing dir")
                .code,
            ErrorCode::NotFound
        );
    }
}
