// SPDX-License-Identifier: MIT OR Apache-2.0
use super::*;
use crate::application::{source, support};
use alef_core::security::{
    authority::Authority, consent::Consent, grants::Grants, permissions::Permission,
};
use std::ffi::OsStr;

const ID: &str = "org.example.app";

fn os_bases(root: &Path) -> (PathBuf, PathBuf, PathBuf) {
    (
        root.join("os-data"),
        root.join("os-config"),
        root.join("os-cache"),
    )
}

fn resolved(root: &Path, override_home: Option<&Path>) -> (PathBuf, PathBuf, PathBuf) {
    application_bases(override_home.map(Path::as_os_str), || os_bases(root))
}

fn unchanged(before: &PathVars, after: &PathVars) {
    assert_eq!(before.home, after.home);
    assert_eq!(before.documents, after.documents);
    assert_eq!(before.downloads, after.downloads);
    assert_eq!(before.desktop, after.desktop);
    assert_eq!(before.temp, after.temp);
    assert_eq!(before.app, after.app);
}

fn owned(vars: &PathVars) -> [&Path; 3] {
    [&vars.app_data, &vars.app_config, &vars.app_cache]
}

fn package(folder: &Path) -> source::Prepared {
    let text = support::manifest_text("");
    let path = folder.join("app.alef");
    std::fs::write(
        &path,
        support::zip(&[
            ("alef.ktav", text.as_bytes()),
            ("dist/index.html", b"<html></html>"),
        ]),
    )
    .expect("package fixture");
    let source::ApplicationSource::Package(prepared) = source::load(&path).expect("package") else {
        panic!("expected package");
    };
    *prepared
}

#[test]
fn resolver_uses_distinct_os_bases_for_unset_empty_and_relative_values() {
    let scratch = tempfile::tempdir().expect("scratch");
    for value in [
        None,
        Some(OsStr::new("")),
        Some(OsStr::new("relative")),
        Some(OsStr::new(".")),
        Some(OsStr::new("a/b")),
    ] {
        let mut called = false;
        let actual = application_bases(value, || {
            called = true;
            os_bases(scratch.path())
        });
        assert!(called, "{value:?}");
        assert_eq!(actual, os_bases(scratch.path()), "{value:?}");
    }
    let home = scratch.path().join("override");
    assert!(home.is_absolute());
    assert_eq!(
        application_bases(Some(home.as_os_str()), || panic!("OS lookup on override")),
        (home.join("data"), home.join("config"), home.join("cache"))
    );
    assert!(!home.exists());
    for base in ["os-data", "os-config", "os-cache"] {
        assert!(!scratch.path().join(base).exists());
    }
}

#[test]
fn directory_override_preserves_user_folders_and_does_not_create_owned_paths() {
    let scratch = tempfile::tempdir().expect("scratch");
    let app = std::fs::canonicalize(scratch.path()).expect("canonical app");
    let user_home = scratch.path().join("user-home");
    let profile = scratch.path().join("profile");
    let baseline = path_vars_with_bases(
        app.clone(),
        ID,
        user_home.clone(),
        resolved(scratch.path(), None),
    )
    .expect("baseline");
    let overridden = path_vars_with_bases(
        app.clone(),
        ID,
        user_home.clone(),
        resolved(scratch.path(), Some(&profile)),
    )
    .expect("override");
    unchanged(&baseline, &overridden);
    assert_eq!(overridden.home, user_home);
    assert_eq!(overridden.app, Some(app));
    for (path, name) in owned(&overridden)
        .into_iter()
        .zip(["data", "config", "cache"])
    {
        assert_eq!(path, profile.join(name).join(ID));
        assert!(!path.exists());
    }
    for (path, name) in owned(&baseline)
        .into_iter()
        .zip(["os-data", "os-config", "os-cache"])
    {
        assert_eq!(path, scratch.path().join(name).join(ID));
        assert!(!path.exists());
    }
    assert!(!profile.exists());
    assert!(!user_home.exists());
}

#[test]
fn invalid_directory_id_precedes_missing_directory_error() {
    let scratch = tempfile::tempdir().expect("scratch");
    let missing = scratch.path().join("missing");
    assert_eq!(
        path_vars(&missing, "../escape")
            .err()
            .expect("invalid id")
            .code,
        ErrorCode::ManifestInvalid
    );
    assert_eq!(
        path_vars(&missing, ID)
            .err()
            .expect("missing directory")
            .code,
        ErrorCode::NotFound
    );
}

#[test]
fn package_override_uses_full_signed_and_unsigned_authority_layout_lazily() {
    let scratch = tempfile::tempdir().expect("scratch");
    let prepared = package(scratch.path());
    assert_eq!(prepared.manifest.id, ID);
    let user_home = scratch.path().join("user-home");
    let profile = scratch.path().join("profile");
    let mut tokens = Vec::new();
    for authority in [
        Authority::SignedPackage {
            fingerprint: "a".repeat(64),
        },
        Authority::UnsignedPackage {
            archive_digest: prepared.archive_digest.clone(),
        },
    ] {
        let token = authority.package_token(ID).expect("token");
        let old = resolved(scratch.path(), None);
        let new = resolved(scratch.path(), Some(&profile));
        let mut baseline =
            package_path_vars_with_bases(ID, user_home.clone(), old.clone()).expect("baseline");
        let mut overridden =
            package_path_vars_with_bases(ID, user_home.clone(), new.clone()).expect("override");
        alef_modules::namespace_paths(&mut baseline, &old.0, &old.1, &old.2, ID, &authority);
        alef_modules::namespace_paths(&mut overridden, &new.0, &new.1, &new.2, ID, &authority);
        unchanged(&baseline, &overridden);
        assert_eq!(overridden.home, user_home);
        assert_eq!(overridden.app, None);
        for (vars, bases) in [(&baseline, old), (&overridden, new)] {
            for (path, base) in owned(vars).into_iter().zip([bases.0, bases.1, bases.2]) {
                assert_eq!(path, base.join("alef-packages").join(&token));
                assert_ne!(path, base.join(ID));
                assert!(!path.exists());
            }
        }
        tokens.push(token);
    }
    assert_ne!(tokens[0], tokens[1]);
    assert!(!profile.exists());
    assert!(!user_home.exists());
}

fn child_file(base: &Path) -> PathBuf {
    let folder = base.join("profile");
    std::fs::create_dir_all(&folder).expect("scope folder");
    let file = folder.join("child.txt");
    std::fs::write(&file, b"scope target").expect("scope child");
    file
}

#[test]
fn all_three_scopes_allow_override_and_deny_old_os_and_unrelated_files_for_both_sources() {
    let scratch = tempfile::tempdir().expect("scratch");
    let app = std::fs::canonicalize(scratch.path()).expect("app");
    let user_home = scratch.path().join("user-home");
    let profile = scratch.path().join("override");
    // Ktav separates list entries with newlines, not spaces.
    let scopes = [
        "$APPDATA/profile/*",
        "$APPCONFIG/profile/*",
        "$APPCACHE/profile/*",
    ];
    let scope = format!("[\n{}\n        ]", scopes.join("\n            "));
    let text = support::manifest_text("")
        .replace("read: []", &format!("read: {scope}"))
        .replace("write: []", &format!("write: {scope}"));
    for (iteration, is_package) in [false, true].into_iter().enumerate() {
        let source = if is_package { "package" } else { "directory" };
        let old = resolved(scratch.path(), None);
        let new = resolved(scratch.path(), Some(&profile));
        let (mut baseline, mut overridden) = if is_package {
            (
                package_path_vars_with_bases(ID, user_home.clone(), old.clone()).expect("baseline"),
                package_path_vars_with_bases(ID, user_home.clone(), new.clone()).expect("override"),
            )
        } else {
            (
                path_vars_with_bases(app.clone(), ID, user_home.clone(), old.clone())
                    .expect("baseline"),
                path_vars_with_bases(app.clone(), ID, user_home.clone(), new.clone())
                    .expect("override"),
            )
        };
        if is_package {
            let authority = Authority::UnsignedPackage {
                archive_digest: "b".repeat(64),
            };
            alef_modules::namespace_paths(&mut baseline, &old.0, &old.1, &old.2, ID, &authority);
            alef_modules::namespace_paths(&mut overridden, &new.0, &new.1, &new.2, ID, &authority);
        }
        let declaration = Manifest::from_ktav_str(&text).expect("manifest");
        assert_eq!(
            declaration.permissions.fs.read, scopes,
            "iteration={iteration} source={source} permission=fs.read"
        );
        assert_eq!(
            declaration.permissions.fs.write, scopes,
            "iteration={iteration} source={source} permission=fs.write"
        );
        let plan = if is_package {
            let prepared = package(scratch.path());
            make_package_plan(prepared.package, "dist", declaration, &overridden)
                .expect("package plan")
        } else {
            make_plan(&app, declaration, &overridden).expect("directory plan")
        };
        let permissions = (*plan.permissions)
            .clone()
            .with_consent(Consent::allow_all());
        let unrelated = child_file(&scratch.path().join("unrelated"));
        for (allowed, denied) in owned(&overridden).into_iter().zip(owned(&baseline)) {
            let allowed = child_file(allowed);
            let denied = child_file(denied);
            for permission in [Permission::FsRead, Permission::FsWrite] {
                let authorized = permissions
                    .authorize_path(permission, allowed.to_str(), &Grants::new())
                    .unwrap_or_else(|error| {
                        panic!(
                            "iteration={iteration} source={source} path={allowed:?} permission={permission:?}: {error:?}"
                        )
                    });
                assert_eq!(
                    std::fs::canonicalize(&authorized).expect("canonical authorized child"),
                    std::fs::canonicalize(&allowed).expect("canonical child"),
                    "iteration={iteration} source={source} path={allowed:?} permission={permission:?}"
                );
                for target in [&denied, &unrelated] {
                    let result =
                        permissions.authorize_path(permission, target.to_str(), &Grants::new());
                    assert!(
                        result.is_err(),
                        "iteration={iteration} source={source} path={target:?} permission={permission:?}: {result:?}"
                    );
                }
            }
        }
    }
}
