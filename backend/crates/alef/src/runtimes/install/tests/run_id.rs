// SPDX-License-Identifier: MIT OR Apache-2.0
//! Resolution of `alef run <ID>` against a fake installed store; the real profile is never touched.
use super::*;
use crate::application::source::LoadFailure;
use std::path::Path;

fn hash_of(id: &str) -> String {
    format!("{:x}", sha2::Sha256::digest(id.as_bytes()))
}

fn write_record(home: &Path, id: &str, version: &str, package: &str) -> PathBuf {
    let app_dir = home.join("apps").join(hash_of(id));
    std::fs::create_dir_all(&app_dir).expect("app directory");
    let record = AppRecord {
        id: id.to_owned(),
        name: "Fixture".to_owned(),
        version: version.to_owned(),
        icon: None,
        package: package.to_owned(),
        executable: home.join("alef"),
        installed_time: 0,
    };
    std::fs::write(
        app_dir.join("app.json"),
        serde_json::to_vec(&record).expect("record json"),
    )
    .expect("write record");
    app_dir
}

#[test]
fn an_installed_id_resolves_to_the_exact_package_file_of_its_record() {
    let home = tempfile::tempdir().expect("tempdir");
    let app_dir = write_record(home.path(), "org.example.app", "1.0.0", "1.0.0.alef");
    std::fs::write(app_dir.join("1.0.0.alef"), b"package").expect("package file");
    let resolved = resolve_installed_in(home.path(), "org.example.app").expect("resolved");
    assert_eq!(resolved, app_dir.join("1.0.0.alef"));
    assert!(resolved.is_file());
}

#[test]
fn a_valid_id_without_a_store_directory_is_not_installed_with_exit_one() {
    let home = tempfile::tempdir().expect("tempdir");
    let error = resolve_installed_in(home.path(), "org.example.app").expect_err("absent");
    assert_eq!(error.category, "NOT_INSTALLED");
    assert_eq!(error.detail, "org.example.app");
    assert_eq!(error.exit, 1);
}

#[test]
fn a_damaged_record_is_a_clear_error_and_leaves_the_store_in_place() {
    let home = tempfile::tempdir().expect("tempdir");
    let app_dir = home.path().join("apps").join(hash_of("org.example.app"));
    std::fs::create_dir_all(&app_dir).expect("app directory");
    std::fs::write(app_dir.join("app.json"), b"{not json").expect("damaged record");
    let error = resolve_installed_in(home.path(), "org.example.app").expect_err("damaged");
    assert_eq!(error.category, "APP_RECORD");
    assert_eq!(error.exit, 1);
    assert_eq!(
        std::fs::read(app_dir.join("app.json")).expect("record unchanged"),
        b"{not json"
    );
}

#[test]
fn a_record_pointing_outside_the_directory_or_at_a_mismatched_version_is_refused() {
    let home = tempfile::tempdir().expect("tempdir");
    let outside = write_record(home.path(), "org.example.app", "1.0.0", "../evil.alef");
    let error = resolve_installed_in(home.path(), "org.example.app").expect_err("outside");
    assert_eq!(error.category, "APP_RECORD");
    let mismatched = write_record(home.path(), "org.other.app", "1.0.0", "2.0.0.alef");
    std::fs::write(mismatched.join("2.0.0.alef"), b"package").expect("package file");
    let error = resolve_installed_in(home.path(), "org.other.app").expect_err("mismatched");
    assert_eq!(error.category, "APP_RECORD");
    let _ = outside;
}

#[cfg(unix)]
#[test]
fn a_linked_package_file_is_refused() {
    use std::os::unix::fs::symlink;
    let home = tempfile::tempdir().expect("tempdir");
    let app_dir = write_record(home.path(), "org.example.app", "1.0.0", "1.0.0.alef");
    let target = home.path().join("outside.alef");
    std::fs::write(&target, b"package").expect("outside file");
    symlink(&target, app_dir.join("1.0.0.alef")).expect("symlink");
    let error = resolve_installed_in(home.path(), "org.example.app").expect_err("linked");
    assert_eq!(error.category, "APP_RECORD");
}

#[test]
fn an_existing_file_named_like_an_id_stays_a_path_and_a_neither_argument_passes_through() {
    let home = tempfile::tempdir().expect("tempdir");
    let app_dir = write_record(home.path(), "org.example.app", "1.0.0", "1.0.0.alef");
    std::fs::write(app_dir.join("1.0.0.alef"), b"package").expect("package file");
    // A real file with an id-shaped name is a path; the record is never consulted.
    let file = home.path().join("org.example.app");
    std::fs::write(&file, b"a directory source candidate").expect("existing file");
    assert!(matches!(
        run_target_in(home.path(), &file, true),
        RunTarget::Path(ref path) if path == &file
    ));
    // A missing id-shaped argument resolves through the store.
    assert!(matches!(
        run_target_in(home.path(), Path::new("org.example.app"), true),
        RunTarget::Id(ref id) if id == "org.example.app"
    ));
    // Neither an existing path nor a valid id keeps the path for the source loader.
    assert!(matches!(
        run_target_in(home.path(), Path::new("../not-an-id"), true),
        RunTarget::Path(ref path) if path == Path::new("../not-an-id")
    ));
}

#[test]
fn the_app_form_never_resolves_an_installed_id() {
    let home = tempfile::tempdir().expect("tempdir");
    let app_dir = write_record(home.path(), "org.example.app", "1.0.0", "1.0.0.alef");
    std::fs::write(app_dir.join("1.0.0.alef"), b"package").expect("package file");
    // The id is installed and no local entry exists, yet `--app` keeps the path.
    assert!(matches!(
        run_target_in(home.path(), Path::new("org.example.app"), false),
        RunTarget::Path(ref path) if path == Path::new("org.example.app")
    ));
}

#[test]
fn internal_subcommand_names_are_valid_installed_ids() {
    let home = tempfile::tempdir().expect("tempdir");
    let app_dir = write_record(home.path(), "trust", "1.0.0", "1.0.0.alef");
    std::fs::write(app_dir.join("1.0.0.alef"), b"package").expect("package file");
    assert!(matches!(
        run_target_in(home.path(), Path::new("trust"), true),
        RunTarget::Id(ref id) if id == "trust"
    ));
    let loaded = run_id_in(home.path(), "trust", &mut |_| Ok("opened")).expect("resolved");
    assert_eq!(loaded, "opened");
}

#[test]
fn a_concurrent_upgrade_reresolves_and_the_new_version_runs() {
    let home = tempfile::tempdir().expect("tempdir");
    let app_dir = write_record(home.path(), "org.example.app", "1.0.0", "1.0.0.alef");
    let old = app_dir.join("1.0.0.alef");
    std::fs::write(&old, b"old").expect("old package");
    let mut attempts = 0;
    let loaded = run_id_in(home.path(), "org.example.app", &mut |resolved| {
        attempts += 1;
        if attempts == 1 {
            // The concurrent upgrade: publish the new record, remove the old package.
            assert_eq!(resolved, &old);
            std::fs::remove_file(&old).expect("remove the old package");
            let upgraded = write_record(home.path(), "org.example.app", "2.0.0", "2.0.0.alef");
            std::fs::write(upgraded.join("2.0.0.alef"), b"new").expect("new package");
            return Err(LoadFailure {
                category: Some("NOT_A_PACKAGE"),
                detail: format!(
                    "{}: expected a format-1 Alef ZIP package",
                    resolved.display()
                ),
                exit: 2,
                runtime_requirement: None,
            });
        }
        assert!(resolved.ends_with("2.0.0.alef"), "{resolved:?}");
        Ok("new version")
    })
    .expect("the new version runs");
    assert_eq!(loaded, "new version");
    assert_eq!(attempts, 2);
}

#[test]
fn a_failure_with_an_unchanged_record_is_the_answer_of_that_one_attempt() {
    let home = tempfile::tempdir().expect("tempdir");
    let app_dir = write_record(home.path(), "org.example.app", "1.0.0", "1.0.0.alef");
    std::fs::write(app_dir.join("1.0.0.alef"), b"package").expect("package file");
    let mut attempts = 0;
    let error = run_id_in::<()>(home.path(), "org.example.app", &mut |_| {
        attempts += 1;
        Err(LoadFailure {
            category: Some("PACKAGE_FORMAT"),
            detail: "broken zip".to_owned(),
            exit: 2,
            runtime_requirement: None,
        })
    })
    .expect_err("the failure is returned");
    assert_eq!(attempts, 1);
    match error {
        RunLaunchFailure::Load(failure) => assert_eq!(failure.category, Some("PACKAGE_FORMAT")),
        RunLaunchFailure::Resolve(_) => panic!("a load failure, not a resolution failure"),
    }
}

#[test]
fn the_retries_are_bounded_and_the_last_error_is_reported() {
    let home = tempfile::tempdir().expect("tempdir");
    let app_dir = write_record(home.path(), "org.example.app", "1.0.0", "1.0.0.alef");
    std::fs::write(app_dir.join("1.0.0.alef"), b"package").expect("package file");
    let mut attempts = 0;
    let error = run_id_in::<()>(home.path(), "org.example.app", &mut |resolved| {
        attempts += 1;
        // Every attempt is raced: the upgrade publishes the next record and removes the
        // package this attempt resolved, so the next attempt sees a changed record.
        let next = format!("{}.0.0", 1 + attempts);
        let upgraded = write_record(
            home.path(),
            "org.example.app",
            &next,
            &format!("{next}.alef"),
        );
        std::fs::write(upgraded.join(format!("{next}.alef")), b"package").expect("package file");
        let _ = std::fs::remove_file(resolved);
        Err(LoadFailure {
            category: Some("NOT_A_PACKAGE"),
            detail: format!("{}: gone", resolved.display()),
            exit: 2,
            runtime_requirement: None,
        })
    })
    .expect_err("every attempt is raced");
    assert_eq!(attempts, 3, "RUN_ATTEMPTS");
    assert!(matches!(error, RunLaunchFailure::Load(_)));
}

#[test]
fn a_record_that_changed_or_vanished_during_an_attempt_is_detected() {
    let home = tempfile::tempdir().expect("tempdir");
    let app_dir = write_record(home.path(), "org.example.app", "1.0.0", "1.0.0.alef");
    let before = record_bytes(home.path(), "org.example.app");
    assert!(!record_changed(home.path(), "org.example.app", &before));
    // The upgrade publishes a new record: the attempt must be retried.
    write_record(home.path(), "org.example.app", "2.0.0", "2.0.0.alef");
    assert!(record_changed(home.path(), "org.example.app", &before));
    // A vanished record is a change too: the attempt must be retried.
    let before = record_bytes(home.path(), "org.example.app");
    let _ = std::fs::remove_file(app_dir.join("app.json"));
    assert!(record_changed(home.path(), "org.example.app", &before));
}

#[cfg(unix)]
#[test]
fn a_linked_store_root_or_app_directory_is_a_damaged_store_error() {
    use std::os::unix::fs::symlink;
    // A linked `apps` root: the same refusal `alef apps` gives, exit 1, not NOT_INSTALLED.
    let home = tempfile::tempdir().expect("tempdir");
    let real = home.path().join("real");
    std::fs::create_dir(&real).expect("real root");
    symlink(&real, home.path().join("apps")).expect("linked root");
    let error = resolve_installed_in(home.path(), "org.example.app").expect_err("linked root");
    assert_eq!(error.category, "APP_STORE");
    assert_eq!(error.exit, 1);
    // A linked application directory is a damaged store, not an absent application.
    let home = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir(home.path().join("apps")).expect("plain root");
    let elsewhere = home.path().join("elsewhere");
    std::fs::create_dir(&elsewhere).expect("elsewhere");
    symlink(
        &elsewhere,
        home.path().join("apps").join(hash_of("org.example.app")),
    )
    .expect("linked app directory");
    let error = resolve_installed_in(home.path(), "org.example.app").expect_err("linked dir");
    assert_eq!(error.category, "APP_STORE");
    assert_eq!(error.exit, 1);
}

#[test]
fn an_application_without_a_store_record_is_not_installed_for_the_updater() {
    let home = tempfile::tempdir().expect("tempdir");
    assert_eq!(
        installed_app(
            home.path(),
            &home.path().join("elsewhere.alef"),
            "org.example.app",
            "1.0.0"
        ),
        None
    );
}

#[test]
fn a_store_record_of_another_version_is_not_the_running_package() {
    let home = tempfile::tempdir().expect("tempdir");
    let app_dir = write_record(home.path(), "org.example.app", "2.0.0", "2.0.0.alef");
    std::fs::write(app_dir.join("2.0.0.alef"), b"package").expect("package file");
    assert_eq!(
        installed_app(
            home.path(),
            &app_dir.join("2.0.0.alef"),
            "org.example.app",
            "1.0.0"
        ),
        None
    );
}

#[test]
fn the_running_store_package_of_a_valid_record_is_installed() {
    let home = tempfile::tempdir().expect("tempdir");
    let app_dir = write_record(home.path(), "org.example.app", "1.0.0", "1.0.0.alef");
    let package = app_dir.join("1.0.0.alef");
    std::fs::write(&package, b"package").expect("package file");
    let installed =
        installed_app(home.path(), &package, "org.example.app", "1.0.0").expect("installed");
    assert_eq!(
        std::fs::canonicalize(&installed.package).unwrap(),
        std::fs::canonicalize(&package).unwrap()
    );
}

#[cfg(unix)]
#[test]
fn a_symlink_inside_the_app_store_is_still_refused() {
    use std::os::unix::fs::symlink;
    let home = tempfile::tempdir().expect("tempdir");
    std::fs::create_dir(home.path().join("apps")).expect("plain store root");
    let elsewhere = tempfile::tempdir().expect("tempdir");
    symlink(
        elsewhere.path(),
        home.path().join("apps").join(hash_of("org.example.app")),
    )
    .expect("symlinked app directory");
    let error = resolve_installed_in(home.path(), "org.example.app").expect_err("linked");
    assert_eq!(error.category, "APP_STORE");
    assert_eq!(
        error.detail,
        "application directory is linked or not a directory"
    );
    // Only the configured root is exempt: a linked prefix below a real root stays refused.
    symlink(elsewhere.path(), home.path().join("apps").join("elsewhere")).expect("second symlink");
    assert_eq!(
        crate::install::store::create_checked_dir(&home.path().join("apps").join("elsewhere"))
            .unwrap_err()
            .kind(),
        std::io::ErrorKind::InvalidData
    );
}
