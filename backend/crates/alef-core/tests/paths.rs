// SPDX-License-Identifier: MIT OR Apache-2.0
//! Filesystem scopes: canonicalization, globbing, symlink resistance, variables and runtime grants.
use alef_core::{
    security::{
        grants::Grants,
        manifest::{Manifest, Permissions},
        permissions::{PathVars, Permission, PermissionSet},
    },
    ErrorCode,
};
use std::{
    fs,
    path::{Path, PathBuf},
};

const CASE_INSENSITIVE: bool = cfg!(any(windows, target_os = "macos"));

/// Scratch directory under the build's own tmp dir, never the system temp.
fn sandbox() -> tempfile::TempDir {
    tempfile::Builder::new()
        .prefix("alef-paths-")
        .tempdir_in(env!("CARGO_TARGET_TMPDIR"))
        .expect("sandbox")
}

fn closed() -> Permissions {
    Manifest::from_ktav_str(include_str!("fixtures/minimal.ktav"))
        .expect("fixture")
        .permissions
}

fn vars(root: &Path) -> PathVars {
    PathVars {
        app_data: root.join("appdata"),
        app_config: root.join("appconfig"),
        app_cache: root.join("appcache"),
        home: root.join("home"),
        documents: root.join("documents"),
        downloads: root.join("downloads"),
        desktop: root.join("desktop"),
        temp: root.join("temp"),
        app: root.join("app"),
    }
}

fn scopes(root: &Path, read: &[&str], write: &[&str]) -> PermissionSet {
    let mut policy = closed();
    policy.fs.read = read
        .iter()
        .map(|s| s.replace("{root}", &root.display().to_string()))
        .collect();
    policy.fs.write = write
        .iter()
        .map(|s| s.replace("{root}", &root.display().to_string()))
        .collect();
    PermissionSet::from_manifest(&policy, &vars(root)).expect("scopes")
}

fn can(set: &PermissionSet, permission: Permission, path: &Path) -> bool {
    set.check(
        permission,
        Some(path.to_str().expect("utf-8")),
        &Grants::new(),
    )
    .is_ok()
}

fn read(set: &PermissionSet, path: &Path) -> bool {
    can(set, Permission::FsRead, path)
}

fn plain(path: PathBuf) -> PathBuf {
    PathBuf::from(
        path.to_string_lossy()
            .trim_start_matches(r"\\?\")
            .to_owned(),
    )
}

/// Directory link (symlink, or a junction on Windows without symlink privilege).
fn link_dir(target: &Path, link: &Path) -> bool {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link).is_ok()
    }
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_dir(target, link).is_ok()
            || std::process::Command::new("cmd")
                .args(["/C", "mklink", "/J"])
                .arg(link)
                .arg(target)
                .output()
                .map(|out| out.status.success())
                .unwrap_or(false)
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (target, link);
        false
    }
}

fn link_file(target: &Path, link: &Path) -> bool {
    #[cfg(unix)]
    {
        std::os::unix::fs::symlink(target, link).is_ok()
    }
    #[cfg(windows)]
    {
        std::os::windows::fs::symlink_file(target, link).is_ok()
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = (target, link);
        false
    }
}

/// A link that cannot be created means the case was NOT exercised: say so loudly, fail under CI.
fn exercised(created: bool, what: &str) -> bool {
    if !created {
        eprintln!("NOT EXERCISED: cannot create {what} on this machine");
        assert!(
            std::env::var_os("CI").is_none(),
            "{what} must be creatable in CI"
        );
    }
    created
}

struct Tree {
    _guard: tempfile::TempDir,
    root: PathBuf,
}

/// `allowed/{real/secret.txt}`, `outside/secret.txt`, `secret/`, `allowed-evil/`.
fn tree() -> Tree {
    let guard = sandbox();
    let root = plain(fs::canonicalize(guard.path()).expect("canonical root"));
    for dir in ["allowed/real", "outside", "secret", "allowed-evil"] {
        fs::create_dir_all(root.join(dir)).expect("dir");
    }
    fs::write(root.join("allowed/real/secret.txt"), b"in").expect("file");
    fs::write(root.join("outside/secret.txt"), b"out").expect("file");
    Tree {
        _guard: guard,
        root,
    }
}

#[test]
fn star_matches_one_component_and_double_star_any_depth() {
    let t = tree();
    let set = scopes(
        &t.root,
        &["{root}/data/**/leaf", "{root}/one/*", "{root}/notes/*.txt"],
        &[],
    );
    let at = |p: &str| t.root.join(p);
    assert!(read(&set, &at("data/leaf")), "** matches zero components");
    assert!(read(&set, &at("data/a/b/leaf")));
    assert!(!read(&set, &at("data/a/b/other")));
    assert!(read(&set, &at("one/file")));
    assert!(!read(&set, &at("one")), "* needs exactly one component");
    assert!(!read(&set, &at("one/a/b")), "* never crosses a separator");
    assert!(read(&set, &at("notes/a.txt")));
    assert!(
        !read(&set, &at("notes/a.md")),
        "component glob keeps the suffix"
    );
    assert!(!read(&set, &at("notes/sub/a.txt")));
}

#[test]
fn plain_path_scope_is_exactly_that_path() {
    let t = tree();
    let set = scopes(&t.root, &["{root}/allowed/real/secret.txt"], &[]);
    assert!(read(&set, &t.root.join("allowed/real/secret.txt")));
    assert!(!read(&set, &t.root.join("allowed/real/secret.txt2")));
    assert!(!read(&set, &t.root.join("allowed/real/secret.txt/child")));
    assert!(!read(&set, &t.root.join("allowed/real")));
}

#[test]
fn traversal_and_sibling_prefix_are_denied() {
    let t = tree();
    let set = scopes(&t.root, &["{root}/allowed/**"], &[]);
    let at = |p: &str| t.root.join(p);
    assert!(read(&set, &at("allowed/real/secret.txt")));
    assert!(
        read(&set, &at("allowed/not-created-yet/new.txt")),
        "missing target inside"
    );
    assert!(
        read(&set, &at("allowed/real/../real/secret.txt")),
        "`..` that stays inside"
    );
    assert!(!read(&set, &at("allowed/../secret")));
    assert!(!read(&set, &at("allowed/real/../../outside/secret.txt")));
    assert!(
        !read(&set, &at("allowed-evil/x")),
        "sibling sharing a name prefix"
    );
    assert!(!read(&set, &at("allowed/missing/../../secret")));
}

#[test]
fn hostile_targets_are_denied() {
    let t = tree();
    let set = scopes(&t.root, &["{root}/**"], &["{root}/**"]);
    for target in [
        "",
        "relative/path",
        "./allowed",
        "allowed/real",
        "nul\0byte",
        "\n",
    ] {
        for permission in [Permission::FsRead, Permission::FsWrite] {
            assert_eq!(
                set.check(permission, Some(target), &Grants::new())
                    .unwrap_err()
                    .code,
                ErrorCode::PermissionDenied,
                "{target:?}"
            );
        }
    }
    let with_nul = format!("{}/allowed\0/x", t.root.display());
    assert!(set
        .check(Permission::FsRead, Some(&with_nul), &Grants::new())
        .is_err());
    assert!(set.check(Permission::FsRead, None, &Grants::new()).is_err());
}

#[test]
fn symlink_inside_scope_pointing_outside_is_denied() {
    let t = tree();
    let link = t.root.join("allowed/link");
    if !exercised(link_dir(&t.root.join("outside"), &link), "directory link") {
        return;
    }
    // `**` would admit any depth: only symlink resolution can deny the escape.
    let set = scopes(&t.root, &["{root}/allowed/**"], &["{root}/allowed/**"]);
    assert!(
        read(&set, &t.root.join("allowed/real/secret.txt")),
        "control: a real file is allowed"
    );
    assert!(!read(&set, &link.join("secret.txt")));
    assert!(!read(&set, &link));
    assert!(
        !can(&set, Permission::FsWrite, &link.join("brand-new.txt")),
        "write of a new file through the link"
    );
}

#[test]
fn dotdot_after_a_missing_component_still_resolves_later_symlinks() {
    let t = tree();
    let link = t.root.join("allowed/link");
    if !exercised(link_dir(&t.root.join("outside"), &link), "directory link") {
        return;
    }
    let set = scopes(&t.root, &["{root}/allowed/**"], &[]);
    // lexically this reduces to allowed/link/secret.txt, but `link` must still be followed
    let sneaky = t.root.join("allowed/missing/../link/secret.txt");
    assert!(!read(&set, &sneaky));
    let benign = t.root.join("allowed/missing/../real/secret.txt");
    assert!(
        read(&set, &benign),
        "control: the same shape without a link is allowed"
    );
}

#[test]
fn file_symlink_and_dangling_symlink_are_denied() {
    let t = tree();
    let file_link = t.root.join("allowed/file-link.txt");
    let dangling = t.root.join("allowed/dangling");
    let made_file = link_file(&t.root.join("outside/secret.txt"), &file_link);
    let made_dangling = link_dir(&t.root.join("outside/does-not-exist"), &dangling);
    let set = scopes(&t.root, &["{root}/allowed/**"], &["{root}/allowed/**"]);
    if exercised(made_file, "file symlink") {
        assert!(!read(&set, &file_link));
    }
    if exercised(made_dangling, "dangling link") {
        assert!(!can(
            &set,
            Permission::FsWrite,
            &dangling.join("created-outside.txt")
        ));
        assert!(!can(&set, Permission::FsWrite, &dangling));
    }
}

#[test]
fn case_rules_follow_the_platform() {
    let t = tree();
    let set = scopes(&t.root, &["{root}/allowed/**"], &[]);
    let upper = PathBuf::from(
        t.root
            .join("allowed/real/NEW.TXT")
            .to_string_lossy()
            .replace("allowed", "ALLOWED"),
    );
    assert_eq!(read(&set, &upper), CASE_INSENSITIVE);
}

#[test]
fn authorize_path_returns_the_canonical_path_to_operate_on() {
    let t = tree();
    let set = scopes(&t.root, &[], &["{root}/allowed/**"]);
    let messy = t.root.join("allowed/real/../real/./new.txt");
    let resolved = set
        .authorize_path(Permission::FsWrite, messy.to_str(), &Grants::new())
        .expect("allowed");
    assert_eq!(resolved, t.root.join("allowed/real/new.txt"));
    let denied = set
        .authorize_path(
            Permission::FsWrite,
            t.root.join("secret/x").to_str(),
            &Grants::new(),
        )
        .unwrap_err();
    assert_eq!(denied.code, ErrorCode::PermissionDenied);
    assert_eq!(denied.details.expect("details")["permission"], "fs.write");
    assert_eq!(
        set.authorize_path(Permission::CliExec, Some("x"), &Grants::new())
            .unwrap_err()
            .code,
        ErrorCode::InvalidArgument
    );
}

#[test]
fn read_and_write_scopes_are_independent() {
    let t = tree();
    let set = scopes(&t.root, &["{root}/allowed/**"], &["{root}/secret/**"]);
    assert!(read(&set, &t.root.join("allowed/real/secret.txt")));
    assert!(!can(
        &set,
        Permission::FsWrite,
        &t.root.join("allowed/real/secret.txt")
    ));
    assert!(can(&set, Permission::FsWrite, &t.root.join("secret/new")));
    assert!(!read(&set, &t.root.join("secret/new")));
}

#[test]
fn variables_expand_only_as_the_first_segment_and_unknown_ones_fail() {
    let t = tree();
    let mk = |pattern: &str| {
        let mut policy = closed();
        policy.fs.read = vec![pattern.to_owned()];
        PermissionSet::from_manifest(&policy, &vars(&t.root))
    };
    let set = mk("$HOME/docs/**").expect("valid");
    assert!(read(&set, &t.root.join("home/docs/a/b")));
    assert!(!read(&set, &t.root.join("home/other")));
    for bad in [
        "$UNKNOWN/file",
        "$HOMEX/file",
        "prefix$HOME/file",
        "x/$HOME/file",
        "relative/**",
        "",
        "$HOME/a**b",
        "$HOME/**/../x",
        "$HOME/\0",
    ] {
        assert_eq!(
            mk(bad).unwrap_err().code,
            ErrorCode::ManifestInvalid,
            "{bad:?}"
        );
    }
}

#[cfg(unix)]
#[test]
fn glob_characters_inside_a_variable_value_stay_literal() {
    let t = tree();
    let weird = t.root.join("we*ird");
    fs::create_dir_all(weird.join("docs")).expect("dir");
    fs::create_dir_all(t.root.join("weXird/docs")).expect("dir");
    let mut v = vars(&t.root);
    v.home = weird.clone();
    let mut policy = closed();
    policy.fs.read = vec!["$HOME/docs/**".into()];
    let set = PermissionSet::from_manifest(&policy, &v).expect("valid");
    assert!(read(&set, &weird.join("docs/a")));
    assert!(
        !read(&set, &t.root.join("weXird/docs/a")),
        "`*` in $HOME must not become a wildcard"
    );
}

#[cfg(windows)]
#[test]
fn windows_aliases_streams_and_devices_are_denied() {
    let t = tree();
    let allowed = t.root.join("allowed");
    let by_scope = scopes(&t.root, &["{root}/**"], &["{root}/**"]);
    let by_grant = scopes(&t.root, &[], &[]);
    let granted = Grants::new();
    granted.grant_read(&allowed).expect("grant");
    let control = allowed.join("file.txt");
    assert!(read(&by_scope, &control), "control via scope");
    assert!(by_grant
        .check(Permission::FsRead, control.to_str(), &granted)
        .is_ok());
    for name in [
        "file.",
        "file ",
        "file.txt:stream",
        "nul",
        "NUL.txt",
        "com1",
        "...",
        "a?b",
        "a*b",
    ] {
        let target = format!("{}\\{name}", allowed.display());
        for permission in [Permission::FsRead, Permission::FsWrite] {
            let none = Grants::new();
            assert!(
                by_scope.check(permission, Some(&target), &none).is_err(),
                "scope {name}"
            );
        }
        assert!(
            by_grant
                .check(Permission::FsRead, Some(&target), &granted)
                .is_err(),
            "grant {name}"
        );
    }
}

#[test]
fn directory_grant_covers_its_subtree_and_nothing_else() {
    let t = tree();
    let set = scopes(&t.root, &[], &[]);
    let grants = Grants::new();
    grants
        .grant_read(&t.root.join("allowed"))
        .expect("grant dir");
    let check = |permission, path: &Path| set.check(permission, path.to_str(), &grants).is_ok();
    assert!(check(Permission::FsRead, &t.root.join("allowed")));
    assert!(check(
        Permission::FsRead,
        &t.root.join("allowed/real/secret.txt")
    ));
    assert!(check(
        Permission::FsRead,
        &t.root.join("allowed/new/deep/file")
    ));
    assert!(
        !check(Permission::FsRead, &t.root.join("secret/x")),
        "sibling"
    );
    assert!(!check(Permission::FsRead, &t.root), "parent");
    assert!(
        !check(Permission::FsRead, &t.root.join("allowed-evil/x")),
        "name-prefix sibling"
    );
    assert!(
        !check(Permission::FsRead, &t.root.join("allowed/../secret/x")),
        "escape via .."
    );
    assert!(
        !check(Permission::FsWrite, &t.root.join("allowed/real/secret.txt")),
        "read grant is not write"
    );
    assert!(!grants.is_empty());
}

#[test]
fn file_grant_covers_only_that_file() {
    let t = tree();
    let set = scopes(&t.root, &[], &[]);
    let grants = Grants::new();
    grants
        .grant_write(&t.root.join("outside/secret.txt"))
        .expect("grant file");
    let check = |permission, path: &Path| set.check(permission, path.to_str(), &grants).is_ok();
    assert!(check(
        Permission::FsWrite,
        &t.root.join("outside/secret.txt")
    ));
    assert!(!check(
        Permission::FsWrite,
        &t.root.join("outside/other.txt")
    ));
    assert!(!check(
        Permission::FsWrite,
        &t.root.join("outside/secret.txt/child")
    ));
    assert!(
        !check(Permission::FsRead, &t.root.join("outside/secret.txt")),
        "write grant is not read"
    );
    // a save-dialog target that does not exist yet is a file grant
    grants
        .grant_write(&t.root.join("outside/new-file.bin"))
        .expect("grant new");
    assert!(check(
        Permission::FsWrite,
        &t.root.join("outside/new-file.bin")
    ));
    assert!(!check(
        Permission::FsWrite,
        &t.root.join("outside/new-file.bin/x")
    ));
}

#[test]
fn grants_reject_unusable_paths_and_do_not_follow_links_out() {
    let t = tree();
    let grants = Grants::new();
    for bad in [Path::new(""), Path::new("relative"), Path::new("nul\0x")] {
        assert_eq!(
            grants.grant_read(bad).unwrap_err().code,
            ErrorCode::InvalidArgument,
            "{bad:?}"
        );
        assert_eq!(
            grants.grant_write(bad).unwrap_err().code,
            ErrorCode::InvalidArgument
        );
    }
    assert!(grants.is_empty(), "failed grants add nothing");
    let link = t.root.join("allowed/link");
    if !exercised(link_dir(&t.root.join("outside"), &link), "directory link") {
        return;
    }
    grants.grant_read(&t.root.join("allowed")).expect("grant");
    let set = scopes(&t.root, &[], &[]);
    assert!(set
        .check(
            Permission::FsRead,
            t.root.join("allowed/real/secret.txt").to_str(),
            &grants
        )
        .is_ok());
    assert!(set
        .check(
            Permission::FsRead,
            link.join("secret.txt").to_str(),
            &grants
        )
        .is_err());
}
