// SPDX-License-Identifier: MIT OR Apache-2.0
//! The protected folders of a Rush installation (the user's AI agent): its configuration and data
//! hold the model keys, so no application scope may name anything inside them.
//!
//! A root protects itself and everything under it. A mere ancestor of a root (`$HOME`,
//! `$HOME/.config`) is not protected by the roots: broad scopes stay askable and are cut at
//! runtime, per operation; the manifest refusal is only for scopes that name a Rush folder
//! directly.
use super::path::{canonical, parts, same};
use std::path::{Path, PathBuf};
use std::sync::OnceLock;

/// A project data directory of Rush (`rush.db` and friends), protected by name wherever it appears.
pub(crate) const PROJECT_DIR: &str = ".rush";
const CONFIG_VAR: &str = "RUSH_GLOBAL_CONFIG";
const DATA_VAR: &str = "RUSH_GLOBAL_DATA";
const CACHE_VAR: &str = "RUSH_CACHE_DIR";
const UNIX_CONFIG: &str = "/etc/rush";

/// The roots for one environment and one machine layout; Rush's own overrides win. The XDG
/// variables are read on every platform exactly like Rush does.
pub(crate) fn roots_of(
    env: &dyn Fn(&str) -> Option<String>,
    home: &Path,
    local: &Path,
) -> Vec<PathBuf> {
    let override_or = |name: &str, xdg: &str, fallback: PathBuf| match env(name) {
        Some(dir) => PathBuf::from(dir),
        // The XDG variable names the user's folder; only its rush folder is protected.
        None => env(xdg).map_or(fallback, |dir| PathBuf::from(dir).join("rush")),
    };
    let config = override_or(
        CONFIG_VAR,
        "XDG_CONFIG_HOME",
        home.join(".config").join("rush"),
    );
    let mut roots = vec![config];
    if cfg!(windows) {
        roots.push(override_or(DATA_VAR, "XDG_DATA_HOME", local.join("rush")));
        roots.push(override_or(
            CACHE_VAR,
            "XDG_CACHE_HOME",
            local.join("rush/cache"),
        ));
    } else {
        roots.push(override_or(
            DATA_VAR,
            "XDG_DATA_HOME",
            home.join(".local/share/rush"),
        ));
        roots.push(override_or(
            CACHE_VAR,
            "XDG_CACHE_HOME",
            home.join(".cache/rush"),
        ));
        roots.push(PathBuf::from(UNIX_CONFIG));
    }
    roots
}

/// The Rush folders of this process, as canonical component vectors.
pub(crate) struct Rush {
    roots: Vec<Vec<String>>,
}

impl Rush {
    /// The configuration of this process, computed once.
    pub(crate) fn config() -> &'static Rush {
        static CONFIG: OnceLock<Rush> = OnceLock::new();
        CONFIG.get_or_init(|| {
            let env = |name: &str| std::env::var(name).ok().filter(|value| !value.is_empty());
            let home = env("HOME")
                .or_else(|| env("USERPROFILE"))
                .map_or_else(PathBuf::new, PathBuf::from);
            let local = env("LOCALAPPDATA")
                .map_or_else(|| home.join("AppData").join("Local"), PathBuf::from);
            Rush::with_roots(roots_of(&env, &home, &local))
        })
    }

    /// Test/analyse with explicit roots.
    pub(crate) fn with_roots(roots: Vec<PathBuf>) -> Rush {
        Rush {
            roots: roots
                .iter()
                .map(|root| {
                    // The roots are absolute, but a root that cannot be resolved (an unreadable
                    // component) still protects what it spells.
                    let resolved = canonical(root).unwrap_or_else(|| root.to_path_buf());
                    parts(&resolved)
                })
                .collect(),
        }
    }

    /// The canonical component vectors of the roots.
    pub(crate) fn component_roots(&self) -> &[Vec<String>] {
        &self.roots
    }

    /// Whether the canonical components reach a Rush folder: inside a root, at a root, or any
    /// component named `.rush`.
    pub(crate) fn protected(&self, components: &[String]) -> bool {
        components.iter().any(|name| same(name, PROJECT_DIR))
            || self.roots.iter().any(|root| {
                components.len() >= root.len()
                    && root.iter().zip(components).all(|(a, b)| same(a, b))
            })
    }
}

/// The process-wide one-ring answer.
pub(crate) fn protected(components: &[String]) -> bool {
    Rush::config().protected(components)
}

/// The first configured root as a path, for tests that build a scope inside it.
#[cfg(test)]
pub(crate) fn first_root_path() -> Option<PathBuf> {
    Rush::config().roots.first().map(|root| {
        let mut path = PathBuf::new();
        for part in root {
            path.push(part);
        }
        path
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn none(_: &str) -> Option<String> {
        None
    }

    #[test]
    fn the_env_overrides_of_rush_win_over_the_defaults() {
        let env = |name: &str| match name {
            "RUSH_GLOBAL_CONFIG" => Some("/w/config".to_owned()),
            "RUSH_GLOBAL_DATA" => Some("/w/data".to_owned()),
            "RUSH_CACHE_DIR" => Some("/w/cache".to_owned()),
            _ => None,
        };
        let roots = roots_of(&env, Path::new("/h"), Path::new("/l"));
        assert_eq!(roots[0], PathBuf::from("/w/config"));
        assert_eq!(roots[1], PathBuf::from("/w/data"));
        assert_eq!(roots[2], PathBuf::from("/w/cache"));
        // A mixed case: the RUSH_* override is absent for config, so the XDG variable stands in
        // and its rush folder, not the whole XDG folder, is the root.
        let env = |name: &str| match name {
            "RUSH_GLOBAL_DATA" => Some("/w/data".to_owned()),
            "RUSH_CACHE_DIR" => Some("/w/cache".to_owned()),
            "XDG_CONFIG_HOME" => Some("/w/xdg-config".to_owned()),
            _ => None,
        };
        let roots = roots_of(&env, Path::new("/h"), Path::new("/l"));
        assert_eq!(roots[0], PathBuf::from("/w/xdg-config/rush"));
        assert_eq!(roots[1], PathBuf::from("/w/data"));
    }

    #[test]
    fn the_xdg_variables_protect_only_their_rush_folder() {
        let env = |name: &str| match name {
            "XDG_CONFIG_HOME" => Some("/x/cfg".to_owned()),
            "XDG_DATA_HOME" => Some("/x/data".to_owned()),
            "XDG_CACHE_HOME" => Some("/x/cache".to_owned()),
            _ => None,
        };
        let roots = roots_of(&env, Path::new("/h"), Path::new("/l"));
        let expected: Vec<PathBuf> = if cfg!(windows) {
            vec![
                PathBuf::from("/x/cfg/rush"),
                PathBuf::from("/x/data/rush"),
                PathBuf::from("/x/cache/rush"),
            ]
        } else {
            vec![
                PathBuf::from("/x/cfg/rush"),
                PathBuf::from("/x/data/rush"),
                PathBuf::from("/x/cache/rush"),
                PathBuf::from(UNIX_CONFIG),
            ]
        };
        assert_eq!(roots, expected);
    }

    #[test]
    fn the_defaults_follow_the_machine_layout() {
        let roots = roots_of(&none, Path::new("/h"), Path::new("/l"));
        let expected: Vec<PathBuf> = if cfg!(windows) {
            vec![
                PathBuf::from("/h/.config/rush"),
                PathBuf::from("/l/rush"),
                PathBuf::from("/l/rush/cache"),
            ]
        } else {
            vec![
                PathBuf::from("/h/.config/rush"),
                PathBuf::from("/h/.local/share/rush"),
                PathBuf::from("/h/.cache/rush"),
                PathBuf::from("/etc/rush"),
            ]
        };
        assert_eq!(roots, expected);
    }

    #[test]
    fn a_root_protects_itself_and_what_lies_inside_but_not_its_parents() {
        let rush = Rush::with_roots(vec![PathBuf::from("/h/.config/rush")]);
        assert!(rush.protected(&parts(Path::new("/h/.config/rush"))));
        assert!(rush.protected(&parts(Path::new("/h/.config/rush/keys/db.json"))));
        assert!(!rush.protected(&parts(Path::new("/h/.config"))));
        assert!(!rush.protected(&parts(Path::new("/h"))));
        assert!(!rush.protected(&parts(Path::new("/h/.config/rush.db"))));
    }

    #[test]
    fn a_dot_rush_folder_is_protected_wherever_it_appears() {
        let rush = Rush::with_roots(vec![PathBuf::from("/w/config")]);
        assert!(rush.protected(&parts(Path::new("/app/project/.rush/rush.db"))));
        assert!(rush.protected(&parts(Path::new("/app/project/.rush/nested/data.json"))));
        assert!(!rush.protected(&parts(Path::new("/app/project/rush.db"))));
        if same(".rush", ".RUSH") {
            assert!(rush.protected(&parts(Path::new("/app/.RUSH/x"))));
        }
    }

    // The protection in the whole: the manifest refusal and the runtime refusal of
    // `PermissionSet`. The expectation is computed through the same API, so the tests do not
    // depend on the machine's Rush layout.
    use crate::security::{
        grants::Grants,
        manifest::Permissions,
        permissions::{PathVars, Permission, PermissionSet},
    };
    use serde_json::json;
    use std::sync::Arc;

    fn vars() -> PathVars {
        let root = std::env::temp_dir();
        PathVars {
            app_data: root.clone(),
            app_config: root.clone(),
            app_cache: root.clone(),
            home: root.clone(),
            documents: root.clone(),
            downloads: root.clone(),
            desktop: root.clone(),
            temp: root.clone(),
            app: Some(root),
        }
    }

    /// A policy with the given `fs.read` scopes and nothing else.
    fn policy_with_read(read: Vec<String>) -> Permissions {
        serde_json::from_value(json!({
            "fs": {"read": read, "write": []},
            "cli": {"exec": []},
            "net": {"http": [], "socket": []},
            "shell": {"openExternal": []},
            "clipboard": {"read": false},
            "shortcut": {"global": false},
            "secrets": false,
            "app": {"env": []},
        }))
        .expect("policy")
    }

    fn set_with_read(read: Vec<String>) -> PermissionSet {
        PermissionSet::from_manifest(&policy_with_read(read), &vars()).expect("permission set")
    }

    /// A policy that declares an AI builtin with filesystem `scopes` and nothing else.
    fn builtin_policy(scopes: Vec<String>) -> Permissions {
        serde_json::from_value(json!({
            "fs": {"read": [], "write": []},
            "cli": {"exec": []},
            "net": {"http": [], "socket": []},
            "shell": {"openExternal": []},
            "clipboard": {"read": false},
            "shortcut": {"global": false},
            "secrets": false,
            "app": {"env": []},
            "ai": {
                "roles": [],
                "tools": [],
                "builtin": [{"name": "fs.read", "scopes": scopes}],
            },
        }))
        .expect("policy")
    }
    #[test]
    fn a_scope_inside_a_rush_folder_is_refused_at_the_manifest() {
        let root = first_root_path().expect("a Rush root");
        let inside = format!("{}/sub/**", root.to_str().expect("utf-8 root"));
        let error = PermissionSet::from_manifest(&policy_with_read(vec![inside]), &vars())
            .expect_err("refused");
        assert_eq!(error.code, crate::ErrorCode::ManifestInvalid);
        assert!(error.message.contains("Rush"), "{}", error.message);

        // The control: the temporary folder is not a folder of Rush and stays askable.
        let temp = std::env::temp_dir()
            .to_str()
            .expect("utf-8 temp")
            .to_owned();
        set_with_read(vec![format!("{temp}/askable/**")]);
    }

    #[test]
    fn an_ai_builtin_scope_inside_a_rush_folder_is_refused_at_the_manifest() {
        let root = first_root_path().expect("a Rush root");
        let inside = format!("{}/sub/**", root.to_str().expect("utf-8 root"));
        let error = PermissionSet::from_manifest(&builtin_policy(vec![inside]), &vars())
            .expect_err("refused");
        assert_eq!(error.code, crate::ErrorCode::ManifestInvalid);
        assert!(error.message.contains("Rush"), "{}", error.message);
    }

    #[test]
    fn a_path_in_a_rush_folder_or_a_dot_rush_folder_is_refused_at_runtime() {
        let grants = Arc::new(Grants::new());
        let set = set_with_read(Vec::new());
        // The seeded roots refuse a path inside a Rush folder even with nothing else asked.
        if let Some(root) = first_root_path() {
            let inside = root.join("alef-test.txt");
            let error = set
                .authorize_path(
                    Permission::FsRead,
                    Some(inside.to_str().expect("utf-8 root")),
                    &grants,
                )
                .expect_err("refused");
            assert_eq!(error.code, crate::ErrorCode::PermissionDenied);
        }

        // A `.rush` component is refused even inside a granted, declared scope.
        let project = std::env::temp_dir().join("alef-rush-project");
        std::fs::create_dir_all(project.join(".rush")).expect("project created");
        std::fs::write(project.join(".rush").join("data.txt"), b"").expect("data written");
        std::fs::write(project.join("src.txt"), b"").expect("source written");
        let set = set_with_read(vec![format!(
            "{}/**",
            project.to_str().expect("utf-8 project")
        )]);
        let error = set
            .authorize_path(
                Permission::FsRead,
                Some(
                    project
                        .join(".rush")
                        .join("data.txt")
                        .to_str()
                        .expect("utf-8 data"),
                ),
                &grants,
            )
            .expect_err("refused");
        assert_eq!(error.code, crate::ErrorCode::PermissionDenied);
        set.authorize_path(
            Permission::FsRead,
            Some(project.join("src.txt").to_str().expect("utf-8 source")),
            &grants,
        )
        .expect("allowed");
    }

    #[test]
    fn a_link_into_a_protected_folder_is_refused() {
        let scratch = tempfile::tempdir().expect("tempdir");
        let secret = scratch.path().join("secret");
        std::fs::create_dir_all(&secret).expect("secret folder created");
        std::fs::write(secret.join("secret.txt"), b"").expect("secret written");
        let app = scratch.path().join("app");
        std::fs::create_dir_all(&app).expect("app folder created");
        std::fs::write(app.join("plain.txt"), b"").expect("plain written");
        #[cfg(unix)]
        std::os::unix::fs::symlink(&secret, app.join("alias")).expect("link");
        // Windows has no junction in std: build it with mklink; a machine without the call skips.
        #[cfg(windows)]
        {
            let built = std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(app.join("alias"))
                .arg(&secret)
                .output();
            match built {
                Ok(output) if output.status.success() => {}
                other => {
                    println!("junction creation unavailable: {other:?}");
                    return;
                }
            }
        }

        let grants = Grants::new();
        grants.grant_read(&app).expect("grant read");
        let set = set_with_read(vec![format!("{}/**", app.to_str().expect("utf-8 app"))])
            .with_protected(&secret);
        let link = app.join("alias").join("secret.txt");
        let error = set
            .authorize_path(
                Permission::FsRead,
                Some(link.to_str().expect("utf-8 link")),
                &grants,
            )
            .expect_err("refused");
        assert_eq!(error.code, crate::ErrorCode::PermissionDenied);
        set.authorize_path(
            Permission::FsRead,
            Some(app.join("plain.txt").to_str().expect("utf-8 plain")),
            &grants,
        )
        .expect("allowed");
    }
}
