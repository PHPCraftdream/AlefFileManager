// SPDX-License-Identifier: MIT OR Apache-2.0
//! What `alef` does for one application directory: the manifest read from `alef.ktav` and the
//! window, permissions and CSP derived from it. A manifest the runtime cannot honour yet is refused
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
        window::{Length, Monitor, WindowDef, WindowPosition},
    },
};

pub const MANIFEST_FILE: &str = "alef.ktav";

/// Origin of the application documents; `build_csp` allows it explicitly (see its docs).
const CSP_APP_ORIGIN: &str = "native://app";

/// The window the manifest declares, in logical pixels.
#[derive(Debug, PartialEq)]
pub struct WindowPlan {
    pub title: String,
    /// Root-relative entry document.
    pub entry: String,
    pub width: f64,
    pub height: f64,
    pub min_size: Option<(f64, f64)>,
    pub max_size: Option<(f64, f64)>,
}

pub struct Plan {
    pub manifest: Manifest,
    pub window: WindowPlan,
    pub permissions: Arc<PermissionSet>,
    pub csp: String,
    pub assets: PathBuf,
}

fn unavailable(message: String) -> AlefError {
    AlefError::new(ErrorCode::NotAvailable, message)
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

fn pixels(length: Length, what: &str) -> Result<f64, AlefError> {
    match length {
        Length::Px(value) => Ok(value),
        Length::Percent(..) => Err(unavailable(format!(
            "{what}: percentage sizes arrive with the window module (M2.2); use pixels"
        ))),
    }
}

fn bound(
    width: Option<Length>,
    height: Option<Length>,
    what: &str,
    missing: f64,
) -> Result<Option<(f64, f64)>, AlefError> {
    if width.is_none() && height.is_none() {
        return Ok(None);
    }
    let width = width
        .map(|value| pixels(value, &format!("{what}Width")))
        .transpose()?;
    let height = height
        .map(|value| pixels(value, &format!("{what}Height")))
        .transpose()?;
    Ok(Some((width.unwrap_or(missing), height.unwrap_or(missing))))
}

fn window_plan(title: &str, definition: &WindowDef) -> Result<WindowPlan, AlefError> {
    if definition.monitor != Monitor::default()
        || definition.position != WindowPosition::default()
        || definition.restore
    {
        return Err(unavailable(format!(
            "windows[0] ({}): monitor, position and restore arrive with the window module (M2.2)",
            definition.label
        )));
    }
    Ok(WindowPlan {
        title: title.to_owned(),
        entry: definition.url.clone(),
        width: pixels(definition.width, "width")?,
        height: pixels(definition.height, "height")?,
        min_size: bound(definition.min_width, definition.min_height, "min", 0.0)?,
        max_size: bound(definition.max_width, definition.max_height, "max", 1.0e6)?,
    })
}

/// Derives the launch plan; `vars` are the directories behind the `$NAME` scope variables.
pub fn make_plan(app_dir: &Path, manifest: Manifest, vars: &PathVars) -> Result<Plan, AlefError> {
    let window = match manifest.windows.as_slice() {
        [] => {
            return Err(AlefError::new(
                ErrorCode::ManifestInvalid,
                "windows: the manifest declares no window",
            ))
        }
        [only] => window_plan(&manifest.name, only)?,
        _ => {
            return Err(unavailable(
                "windows: several windows arrive with the window module (M2.2); declare one"
                    .to_owned(),
            ))
        }
    };
    let permissions = Arc::new(PermissionSet::from_manifest(&manifest.permissions, vars)?);
    let csp = build_csp(&manifest.external, CSP_APP_ORIGIN)?;
    Ok(Plan {
        window,
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

    fn refused(window: &str) -> AlefError {
        plan_of(manifest(window, "[]"))
            .err()
            .expect("must be refused")
    }

    #[test]
    fn a_plain_manifest_becomes_a_window_a_permission_set_and_a_csp() {
        let plan = plan_of(manifest(SIZE, "[ $APP/data/* ]")).expect("plan");
        assert_eq!(
            plan.window,
            WindowPlan {
                title: "Example".to_owned(),
                entry: "/index.html".to_owned(),
                width: 800.0,
                height: 600.0,
                min_size: None,
                max_size: None,
            }
        );
        assert!(plan
            .csp
            .starts_with("default-src 'none'; script-src 'self' native://app;"));
        assert!(plan.csp.contains("connect-src 'self' native:"));
        assert_eq!(plan.manifest.id, "org.example.app");
    }

    #[test]
    fn min_and_max_sizes_fill_the_missing_side() {
        let plan = plan_of(manifest(
            &format!(
                "{SIZE}        minWidth: 400
        maxHeight: 900
"
            ),
            "[]",
        ))
        .expect("plan");
        assert_eq!(plan.window.min_size, Some((400.0, 0.0)));
        assert_eq!(plan.window.max_size, Some((1.0e6, 900.0)));
    }

    #[test]
    fn what_the_runtime_cannot_honour_yet_is_refused_not_ignored() {
        for (window, mentioned) in [
            (
                "        width: 70%work
        height: 600
",
                "width",
            ),
            (
                "        width: 800
        height: 50%screen
",
                "height",
            ),
            (
                &format!(
                    "{SIZE}        minWidth: 10%screen
"
                ),
                "minWidth",
            ),
            (
                &format!(
                    "{SIZE}        maxHeight: 90%work
"
                ),
                "maxHeight",
            ),
            (
                &format!(
                    "{SIZE}        monitor: cursor
"
                ),
                "monitor",
            ),
            (
                &format!(
                    "{SIZE}        restore: true
"
                ),
                "restore",
            ),
            (
                &format!(
                    "{SIZE}        position: {{ x: 10, y: 20 }}
"
                ),
                "position",
            ),
        ] {
            let error = refused(window);
            assert_eq!(error.code, ErrorCode::NotAvailable, "{window}");
            assert!(error.message.contains(mentioned), "{}", error.message);
            assert!(error.message.contains("M2.2"), "{}", error.message);
        }
    }

    #[test]
    fn exactly_one_window_is_required() {
        let mut none = manifest(SIZE, "[]");
        none.windows.clear();
        let error = plan_of(none).err().expect("no window");
        assert_eq!(error.code, ErrorCode::ManifestInvalid);
        assert!(error.message.starts_with("windows:"));

        let mut two = manifest(SIZE, "[]");
        two.windows.push(two.windows[0].clone());
        let error = plan_of(two).err().expect("two windows");
        assert_eq!(error.code, ErrorCode::NotAvailable);
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
