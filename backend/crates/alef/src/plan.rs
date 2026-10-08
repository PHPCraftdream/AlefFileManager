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
        manifest::Manifest,
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
}

impl Plan {
    /// An application without windows (a console utility or a service) runs in a hidden WebView.
    pub fn windowless(&self) -> bool {
        self.manifest.windows.is_empty()
    }

    /// The entry document: the URL of the first window, or the `entry` of an application without windows.
    pub fn entry(&self) -> String {
        match self.manifest.windows.first() {
            Some(window) => window.url.clone(),
            None => self
                .manifest
                .entry
                .clone()
                .unwrap_or_else(|| "/".to_owned()),
        }
    }
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
    let permissions = Arc::new(PermissionSet::from_manifest(&manifest.permissions, vars)?);
    let csp = build_csp(&manifest.external, CSP_APP_ORIGIN)?;
    Ok(Plan {
        permissions,
        csp,
        assets: app_dir.to_path_buf(),
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
    fn a_plain_manifest_becomes_a_permission_set_and_a_csp() {
        let plan = plan_of(manifest(SIZE, "[ $APP/data/* ]")).expect("plan");
        assert_eq!(plan.manifest.windows.len(), 1);
        assert_eq!(plan.manifest.windows[0].url, "/index.html");
        assert!(plan
            .csp
            .starts_with("default-src 'none'; script-src 'self' native://app;"));
        assert!(plan.csp.contains("connect-src 'self' native:"));
        assert_eq!(plan.manifest.id, "org.example.app");
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
