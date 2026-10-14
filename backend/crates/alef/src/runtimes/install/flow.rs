// SPDX-License-Identifier: MIT OR Apache-2.0
//! Transactional installation of verified application packages.
use std::{
    io::{self, Read},
    path::{Path, PathBuf},
    time::{Duration, SystemTime, UNIX_EPOCH},
};

use alef_core::security::manifest::{compare_versions, validate_package_version};
use sha2::{Digest, Sha256};

use crate::{
    application::{
        pipeline,
        plan::{application_base_dirs, make_package_plan, package_path_vars, Plan},
        source::{Prepared, ARCHIVE_LIMIT, ICON_LIMIT},
        trust::{decide_with_asker, TrustAsker, TrustFailure, TrustInputs},
    },
    consent::ask::AppSummary,
};

use super::{
    integration::{Artifact, Integration},
    store::{create_checked_dir, publish_bytes_no_replace, replace_record, AppLock},
    AppRecord, InstallError, RECORD_LIMIT,
};

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum InstallOutcome {
    Installed(AppRecord),
    AlreadyInstalled(AppRecord),
}

fn error(category: &'static str, detail: impl Into<String>, code: u8) -> InstallError {
    InstallError::new(category, detail, code)
}
fn io_error(err: io::Error) -> InstallError {
    if err.kind() == io::ErrorKind::TimedOut {
        error("BUSY", "application installation is busy", 1)
    } else {
        error("APP_STORE", err.to_string(), 1)
    }
}
fn load_error(err: crate::application::source::LoadFailure) -> InstallError {
    error(err.category.unwrap_or("APP_STORE"), err.detail, err.exit)
}
fn trust_error(err: TrustFailure) -> InstallError {
    match err {
        TrustFailure::BadTrustKey(s) => error("INVALID_ARGUMENT", s, 2),
        TrustFailure::Refused(s) => {
            let (category, detail) = s
                .split_once(':')
                .map(|(c, d)| (c.trim(), d.trim()))
                .filter(|(c, _)| {
                    matches!(
                        *c,
                        "SIGNER_CHANGED" | "SIGNATURE_REQUIRED" | "TRUST_REFUSED"
                    )
                })
                .unwrap_or(("TRUST_REFUSED", s.as_str()));
            let category = match category {
                "SIGNER_CHANGED" => "SIGNER_CHANGED",
                "SIGNATURE_REQUIRED" => "SIGNATURE_REQUIRED",
                _ => "TRUST_REFUSED",
            };
            error(category, detail, 3)
        }
        TrustFailure::Store(e) => io_error(e),
    }
}
fn record_bytes(record: &AppRecord) -> Result<Vec<u8>, InstallError> {
    let bytes = serde_json::to_vec(record).map_err(|e| error("APP_STORE", e.to_string(), 1))?;
    if bytes.len() as u64 > RECORD_LIMIT {
        return Err(error("APP_STORE", "record exceeds size limit", 1));
    }
    Ok(bytes)
}
fn artifact(record: &AppRecord, dir: &Path, executable: &Path) -> Artifact {
    Artifact {
        id: record.id.clone(),
        name: record.name.clone(),
        hash: format!("{:x}", Sha256::digest(record.id.as_bytes())),
        executable: executable.to_owned(),
        package: dir.join(&record.package),
        icon: record.icon.as_ref().map(|_| dir.join("icon.png")),
    }
}
fn bounded_file_bytes(path: &Path, limit: u64) -> Result<Option<Vec<u8>>, InstallError> {
    let mut file = match std::fs::File::open(path) {
        Ok(file) => file,
        Err(e) if e.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(e) => return Err(io_error(e)),
    };
    let mut bytes = Vec::new();
    file.by_ref()
        .take(limit + 1)
        .read_to_end(&mut bytes)
        .map_err(io_error)?;
    if bytes.len() as u64 > limit {
        return Err(error("APP_STORE", "stored file exceeds size limit", 1));
    }
    Ok(Some(bytes))
}

fn plain_file_bytes(path: &Path) -> Result<Option<Vec<u8>>, InstallError> {
    match std::fs::symlink_metadata(path) {
        Ok(meta) if meta.is_file() && crate::runtimes::store::is_regular_file(path) => {
            let mut file = std::fs::File::open(path).map_err(io_error)?;
            let mut bytes = Vec::new();
            file.by_ref()
                .take(ICON_LIMIT + 1)
                .read_to_end(&mut bytes)
                .map_err(io_error)?;
            if bytes.len() as u64 > ICON_LIMIT {
                return Err(error("APP_STORE", "icon exceeds size limit", 1));
            }
            Ok(Some(bytes))
        }
        Ok(_) => Err(error("APP_STORE", "icon path is not a plain file", 1)),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
        Err(e) => Err(io_error(e)),
    }
}
struct NewAppDirGuard {
    dir: PathBuf,
    owned: bool,
}
impl Drop for NewAppDirGuard {
    fn drop(&mut self) {
        if self.owned {
            cleanup_owned_app_dir(&self.dir);
        }
    }
}

fn cleanup_owned_app_dir(dir: &Path) {
    for name in ["app.json", "icon.png", ".lock"] {
        let _ = std::fs::remove_file(dir.join(name));
    }
    if let Ok(entries) = std::fs::read_dir(dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "alef") {
                let _ = std::fs::remove_file(path);
            }
        }
    }
    let _ = std::fs::remove_dir(dir);
}

fn rollback_install(
    integration: &mut dyn Integration,
    id: &str,
    previous: Option<&AppRecord>,
    app_dir: &Path,
    executable: &Path,
    package: &Path,
    icon_path: &Path,
    previous_icon: Option<&[u8]>,
) -> io::Result<()> {
    let mut errors = Vec::new();
    if let Some(icon) = previous_icon {
        if let Err(e) = replace_record(app_dir, icon_path, icon) {
            errors.push(format!("restore icon: {e}"));
        }
    } else if let Err(e) = std::fs::remove_file(icon_path) {
        if e.kind() != io::ErrorKind::NotFound {
            errors.push(format!("remove new icon: {e}"));
        }
    }
    if let Some(previous) = previous {
        if let Err(e) = integration.install(&artifact(previous, app_dir, executable)) {
            errors.push(format!("restore previous integration: {e}"));
        }
    } else if let Err(e) = integration.remove(id, None) {
        errors.push(format!("remove new integration: {e}"));
    }
    if let Err(e) = std::fs::remove_file(package) {
        if e.kind() != io::ErrorKind::NotFound {
            errors.push(format!("remove new package: {e}"));
        }
    }
    if errors.is_empty() {
        Ok(())
    } else {
        Err(io::Error::other(errors.join("; ")))
    }
}

pub async fn install_in(
    home: &Path,
    executable: &Path,
    prepared: Prepared,
    bytes: Vec<u8>,
    inputs: &TrustInputs,
    asker: &TrustAsker,
    integration: &mut dyn Integration,
) -> Result<InstallOutcome, InstallError> {
    if !home.is_absolute() || !executable.is_absolute() {
        return Err(error(
            "INVALID_ARGUMENT",
            "home and executable paths must be absolute",
            2,
        ));
    }
    if bytes.len() as u64 > ARCHIVE_LIMIT {
        return Err(error(
            "PACKAGE_RUN_LIMIT",
            "archive bytes exceed size limit",
            2,
        ));
    }
    if format!("{:x}", Sha256::digest(&bytes)) != prepared.archive_digest {
        return Err(error(
            "PACKAGE_INVALID",
            "archive bytes do not match the verified snapshot",
            2,
        ));
    }
    if prepared
        .icon_png
        .as_ref()
        .is_some_and(|b| b.len() as u64 > ICON_LIMIT)
    {
        return Err(error("PACKAGE_RUN_LIMIT", "icon exceeds size limit", 2));
    }
    let Prepared {
        manifest,
        signer,
        authority,
        archive_digest,
        icon_png,
        public_key,
        package,
        selected_root,
        ..
    } = prepared;
    let id = manifest.id.clone();
    let display_name = if manifest.name.is_empty() {
        manifest.id.clone()
    } else {
        manifest.name.clone()
    };
    let summary = AppSummary {
        id: id.clone(),
        name: display_name.clone(),
        version: manifest.version.clone(),
    };
    let version = manifest.version.clone();
    if !super::valid_id(&id) || validate_package_version(&version).is_err() {
        return Err(error(
            "INVALID_ARGUMENT",
            "invalid application id or version",
            2,
        ));
    }
    let key_fingerprint = match signer {
        alef_package::Signer::Signed { fingerprint, .. } => {
            public_key.map(|key| (key, fingerprint))
        }
        alef_package::Signer::Unsigned => None,
    };
    let mut vars = package_path_vars(&id).map_err(|e| error("MANIFEST_INVALID", e.message, 2))?;
    let (data_base, config_base, cache_base) = application_base_dirs(&vars.home);
    alef_modules::namespace_paths(
        &mut vars,
        &data_base,
        &config_base,
        &cache_base,
        &id,
        &authority,
    );
    if [&vars.app_data, &vars.app_config, &vars.app_cache]
        .iter()
        .any(|path| !path.is_absolute())
    {
        return Err(error(
            "INVALID_ARGUMENT",
            "package namespace paths must be absolute",
            2,
        ));
    }
    let mut plan: Plan = make_package_plan(package, &selected_root, manifest.clone(), &vars)
        .map_err(|e| error("MANIFEST_INVALID", e.to_string(), 2))?;
    let _assets = pipeline::prepare_documents(&mut plan, None)
        .await
        .map_err(load_error)?;
    let hash = format!("{:x}", Sha256::digest(id.as_bytes()));
    let root = home.join("apps");
    create_checked_dir(&root).map_err(io_error)?;
    let root_lock = AppLock::acquire_named(&root, &format!(".{hash}.lock"), Duration::from_secs(2))
        .map_err(io_error)?;
    let app_dir = root.join(&hash);
    let existed = app_dir.exists();
    create_checked_dir(&app_dir).map_err(io_error)?;
    let mut new_dir_guard = NewAppDirGuard {
        dir: app_dir.clone(),
        owned: !existed,
    };
    let app_lock = match AppLock::acquire(&app_dir, Duration::from_secs(2)) {
        Ok(lock) => lock,
        Err(e) => {
            if !existed {
                cleanup_owned_app_dir(&app_dir);
                new_dir_guard.owned = false;
            }
            return Err(io_error(e));
        }
    };
    if !crate::runtimes::store::is_plain_directory(&app_dir) {
        drop(app_lock);
        return Err(error("APP_STORE", "application directory is not plain", 1));
    }
    let record_path = app_dir.join("app.json");
    let old = match std::fs::symlink_metadata(&record_path) {
        Ok(_) => match super::read_record(&app_dir, &hash) {
            Ok(record) => Some(record),
            Err(error) => {
                drop(app_lock);
                return Err(error);
            }
        },
        Err(e) if e.kind() == io::ErrorKind::NotFound => None,
        Err(e) => {
            drop(app_lock);
            return Err(io_error(e));
        }
    };
    let package_name = format!("{version}.alef");
    let package_path = app_dir.join(&package_name);
    if let Some(old_record) = &old {
        match compare_versions(&version, &old_record.version)
            .map_err(|e| error("APP_RECORD", e.to_string(), 1))?
        {
            std::cmp::Ordering::Less => {
                drop(app_lock);
                return Err(error("DOWNGRADE", "installed version is newer", 2));
            }
            std::cmp::Ordering::Equal
                if old_record.version == version
                    && bounded_file_bytes(&app_dir.join(&old_record.package), ARCHIVE_LIMIT)
                        .map_err(|e| e)?
                        .as_deref()
                        == Some(bytes.as_slice()) =>
            {
                decide_with_asker(
                    &home.join("trust"),
                    &id,
                    &summary,
                    key_fingerprint
                        .as_ref()
                        .map(|(key, fingerprint)| (key.as_str(), fingerprint.as_str())),
                    &archive_digest,
                    inputs,
                    asker,
                )
                .map_err(trust_error)?;
                return Ok(InstallOutcome::AlreadyInstalled(old_record.clone()));
            }
            std::cmp::Ordering::Equal => {
                return Err(error(
                    "ALREADY_INSTALLED",
                    "equal-precedence version has different package bytes",
                    2,
                ))
            }
            std::cmp::Ordering::Greater => (),
        }
    }
    match std::fs::symlink_metadata(&package_path) {
        Ok(_) => {
            return Err(error(
                "ALREADY_INSTALLED",
                "version package path already exists",
                2,
            ));
        }
        Err(e) if e.kind() == io::ErrorKind::NotFound => {}
        Err(e) => return Err(io_error(e)),
    }
    let record = AppRecord {
        id: id.clone(),
        name: display_name.clone(),
        version: version.clone(),
        icon: icon_png.as_ref().map(|_| "icon.png".into()),
        package: package_name.clone(),
        executable: executable.to_owned(),
        installed_time: SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs(),
    };
    let new_record = match record_bytes(&record) {
        Ok(bytes) => bytes,
        Err(e) => {
            drop(app_lock);
            return Err(e);
        }
    };
    let icon_path = app_dir.join("icon.png");
    let old_icon = match plain_file_bytes(&icon_path) {
        Ok(icon) => icon,
        Err(e) => {
            drop(app_lock);
            return Err(e);
        }
    };
    decide_with_asker(
        &home.join("trust"),
        &id,
        &summary,
        key_fingerprint
            .as_ref()
            .map(|(key, fingerprint)| (key.as_str(), fingerprint.as_str())),
        &archive_digest,
        inputs,
        asker,
    )
    .map_err(trust_error)?;
    publish_bytes_no_replace(&app_dir, &package_path, &bytes).map_err(|e| {
        if e.kind() == io::ErrorKind::AlreadyExists {
            error("ALREADY_INSTALLED", "package already exists", 2)
        } else {
            io_error(e)
        }
    })?;
    let icon_result = match icon_png.as_deref() {
        Some(icon) => replace_record(&app_dir, &icon_path, icon),
        None if old_icon.is_some() => std::fs::remove_file(&icon_path),
        None => Ok(()),
    };
    if let Err(e) = icon_result {
        let _ = std::fs::remove_file(&package_path);
        drop(app_lock);
        return Err(io_error(e));
    }
    let next = artifact(&record, &app_dir, executable);
    if let Err(e) = integration.install(&next) {
        let rollback = rollback_install(
            integration,
            &id,
            old.as_ref(),
            &app_dir,
            executable,
            &package_path,
            &icon_path,
            old_icon.as_deref(),
        );
        drop(app_lock);
        return Err(match rollback {
            Ok(()) => io_error(e),
            Err(rollback) => error(
                "APP_STORE",
                format!("integration install failed ({e}); rollback failed ({rollback})"),
                1,
            ),
        });
    }
    if let Err(e) = replace_record(&app_dir, &record_path, &new_record) {
        let rollback = rollback_install(
            integration,
            &id,
            old.as_ref(),
            &app_dir,
            executable,
            &package_path,
            &icon_path,
            old_icon.as_deref(),
        );
        drop(app_lock);
        if let Err(rollback_error) = rollback {
            return Err(error(
                "APP_STORE",
                format!("record publication failed ({e}); rollback failed ({rollback_error})"),
                1,
            ));
        }
        return Err(io_error(e));
    }
    new_dir_guard.owned = false;
    if let Some(previous) = old {
        if previous.package != package_name {
            let previous_path = app_dir.join(&previous.package);
            if crate::runtimes::store::is_regular_file(&previous_path) {
                std::fs::remove_file(previous_path).map_err(io_error)?;
            }
        }
    }
    if let Ok(entries) = std::fs::read_dir(&app_dir) {
        for entry in entries.flatten() {
            let path = entry.path();
            if path.extension().is_some_and(|ext| ext == "alef") {
                let Some(name) = path.file_stem().and_then(|s| s.to_str()) else {
                    continue;
                };
                if compare_versions(name, &version)
                    .is_ok_and(|order| order == std::cmp::Ordering::Less)
                {
                    std::fs::remove_file(path).map_err(io_error)?;
                }
            }
        }
    }
    drop(app_lock);
    drop(root_lock);
    Ok(InstallOutcome::Installed(record))
}
