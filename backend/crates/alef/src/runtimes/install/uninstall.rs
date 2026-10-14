// SPDX-License-Identifier: MIT OR Apache-2.0
//! Removal of an installed package and its optionally owned application state.
use super::{integration::Integration, store::AppLock, InstallError};
use crate::application::{plan::package_path_vars, source, trust};
use alef_core::security::{
    authority::Authority,
    consent::{ConsentStore, FileConsentStore, Identity},
};
use sha2::{Digest, Sha256};
use std::{
    collections::HashSet,
    io,
    path::{Path, PathBuf},
    time::Duration,
};

fn err(detail: impl Into<String>) -> InstallError {
    InstallError::new("APP_STORE", detail, 1)
}
fn ioe(e: io::Error) -> InstallError {
    err(e.to_string())
}
fn lock_error(e: io::Error) -> InstallError {
    if e.kind() == io::ErrorKind::TimedOut {
        InstallError::new("BUSY", "BUSY", 1)
    } else {
        ioe(e)
    }
}
fn plain_dir(path: &Path) -> bool {
    crate::runtimes::store::is_plain_directory(path)
}
fn present(path: &Path) -> Result<bool, InstallError> {
    match std::fs::symlink_metadata(path) {
        Ok(_) => Ok(true),
        Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(e) => Err(ioe(e)),
    }
}
fn hex64(s: &str) -> bool {
    s.len() == 64
        && s.bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}
fn scan(path: &Path, depth: usize, found: &mut Vec<PathBuf>) -> Result<(), InstallError> {
    if depth > 64 {
        return Err(err("application tree exceeds removal depth limit"));
    }
    let meta = std::fs::symlink_metadata(path).map_err(ioe)?;
    if meta.file_type().is_symlink() {
        return Err(err("link in removal target"));
    }
    #[cfg(windows)]
    {
        use std::os::windows::fs::MetadataExt;
        if meta.file_attributes() & 0x400 != 0 {
            return Err(err("reparse point in removal target"));
        }
    }
    found.push(path.to_owned());
    if meta.is_dir() {
        if !plain_dir(path) {
            return Err(err("non-plain directory in removal target"));
        }
        for entry in std::fs::read_dir(path).map_err(ioe)? {
            scan(&entry.map_err(ioe)?.path(), depth + 1, found)?;
        }
    } else if !crate::runtimes::store::is_regular_file(path) {
        return Err(err("non-regular object in removal target"));
    }
    Ok(())
}
fn collect_path(path: &Path, found: &mut Vec<PathBuf>) -> Result<(), InstallError> {
    if present(path)? {
        let mut ancestor = path.parent();
        while let Some(parent) = ancestor {
            if present(parent)? && !plain_dir(parent) {
                return Err(err("linked or non-directory ancestor in removal target"));
            }
            ancestor = parent.parent();
        }
        scan(path, 0, found)?;
    }
    Ok(())
}
fn remove_scanned(paths: &[PathBuf]) -> Result<(), InstallError> {
    for p in paths.iter().rev() {
        let meta = std::fs::symlink_metadata(p).map_err(ioe)?;
        if meta.is_dir() {
            std::fs::remove_dir(p).map_err(ioe)?;
        } else {
            std::fs::remove_file(p).map_err(ioe)?;
        }
    }
    Ok(())
}
fn add_identity(ids: &mut Vec<Identity>, identity: Identity) {
    if !ids.contains(&identity) {
        ids.push(identity);
    }
}
fn dedup(paths: Vec<PathBuf>) -> Vec<PathBuf> {
    let mut seen = HashSet::new();
    paths
        .into_iter()
        .filter(|p| seen.insert(p.clone()))
        .collect()
}

/// Uninstalls an application while holding its stable hash gate for the entire transaction.
pub fn uninstall_in(
    home: &Path,
    id: &str,
    purge: bool,
    integration: &mut dyn Integration,
    bases: &(PathBuf, PathBuf, PathBuf),
    announce: &mut dyn FnMut(&Path) -> io::Result<()>,
) -> Result<(), InstallError> {
    if !home.is_absolute() || !super::valid_id(id) {
        return Err(InstallError::new(
            "INVALID_ARGUMENT",
            "invalid home or application id",
            2,
        ));
    }
    if !bases.0.is_absolute() || !bases.1.is_absolute() || !bases.2.is_absolute() {
        return Err(InstallError::new(
            "INVALID_ARGUMENT",
            "application base paths must be absolute",
            2,
        ));
    }
    let hash = format!("{:x}", Sha256::digest(id.as_bytes()));
    let root = home.join("apps");
    if !present(&root)? {
        super::store::create_checked_dir(&root).map_err(ioe)?;
    }
    if !plain_dir(&root) {
        return Err(err("application store root is not plain"));
    }
    let gate = AppLock::acquire_named(&root, &format!(".{hash}.lock"), Duration::from_secs(2))
        .map_err(lock_error)?;
    let dir = root.join(&hash);
    let dir_exists = present(&dir)?;
    if dir_exists && !plain_dir(&dir) {
        return Err(err("application directory is linked or not plain"));
    }
    let lock = if dir_exists {
        Some(AppLock::acquire(&dir, Duration::from_secs(2)).map_err(lock_error)?)
    } else {
        None
    };
    let record = if dir_exists {
        super::read_record(&dir, &hash).ok().filter(|r| r.id == id)
    } else {
        None
    };
    let mut identities = Vec::<Identity>::new();
    let mut targets = Vec::<PathBuf>::new();
    if purge {
        if dir_exists {
            for entry in std::fs::read_dir(&dir).map_err(ioe)? {
                let path = entry.map_err(ioe)?.path();
                if path.extension().and_then(|e| e.to_str()) == Some("alef")
                    && crate::runtimes::store::is_regular_file(&path)
                {
                    if let Ok((prepared, _)) = source::load_install(&path) {
                        if prepared.manifest.id == id {
                            add_identity(&mut identities, prepared.identity());
                        }
                    }
                }
            }
        }
        let store = FileConsentStore::new(home.join("consent"));
        for identity in store
            .identities()
            .map_err(ioe)?
            .into_iter()
            .filter(|i| i.app_id == id)
        {
            let valid = match identity
                .signer
                .as_deref()
                .and_then(|s| s.strip_prefix("pkg-signed:"))
            {
                Some(fp) => hex64(fp),
                None => identity
                    .signer
                    .as_deref()
                    .and_then(|s| s.strip_prefix("pkg-unsigned:"))
                    .is_some_and(hex64),
            };
            if valid {
                add_identity(&mut identities, identity);
            }
        }
        if let Some(pin) = trust::load(&home.join("trust"), id).map_err(ioe)? {
            if !hex64(&pin.fingerprint) {
                return Err(err("invalid trust fingerprint"));
            }
            add_identity(
                &mut identities,
                Identity {
                    app_id: id.to_owned(),
                    signer: Some(format!("pkg-signed:{}", pin.fingerprint)),
                },
            );
        }
        for identity in &identities {
            let authority = match identity.signer.as_deref() {
                Some(s) if s.strip_prefix("pkg-signed:").is_some_and(hex64) => {
                    Authority::SignedPackage {
                        fingerprint: s.strip_prefix("pkg-signed:").unwrap().to_owned(),
                    }
                }
                Some(s) if s.strip_prefix("pkg-unsigned:").is_some_and(hex64) => {
                    Authority::UnsignedPackage {
                        archive_digest: s.strip_prefix("pkg-unsigned:").unwrap().to_owned(),
                    }
                }
                _ => continue,
            };
            let mut paths = package_path_vars(id).map_err(|e| err(e.message))?;
            alef_modules::namespace_paths(&mut paths, &bases.0, &bases.1, &bases.2, id, &authority);
            for p in [&paths.app_data, &paths.app_config, &paths.app_cache] {
                collect_path(p, &mut targets)?;
            }
            let shadow = home.join("shadow").join(identity.key());
            collect_path(&shadow, &mut targets)?;
            let consent_file = home
                .join("consent")
                .join(format!("{}.json", identity.key()));
            collect_path(&consent_file, &mut targets)?;
        }
        let pin_path = trust::pin_path(&home.join("trust"), id);
        collect_path(&pin_path, &mut targets)?;
    }
    targets = dedup(targets);
    let mut tree = Vec::new();
    if dir_exists {
        scan(&dir, 0, &mut tree)?;
    }
    if !dir_exists && targets.is_empty() {
        if !integration.remove(id, None).map_err(ioe)? {
            return Err(InstallError::new(
                "NOT_INSTALLED",
                "application is not installed",
                1,
            ));
        }
        drop(gate);
        return Ok(());
    }
    // Fail safely if the integration lookup reports no artifact; use the persisted record name, not its executable path.
    let removed = integration
        .remove(id, record.as_ref().map(|r| r.name.as_str()))
        .map_err(ioe)?;
    if !removed && !dir_exists && targets.is_empty() {
        return Err(InstallError::new(
            "NOT_INSTALLED",
            "application artifacts were not installed",
            1,
        ));
    }
    for path in &targets {
        announce(path).map_err(|e| err(format!("announce {}: {e}", path.display())))?;
    }
    drop(lock);
    if dir_exists {
        remove_scanned(&tree)?;
    }
    remove_scanned(&targets)?;
    drop(gate);
    Ok(())
}
