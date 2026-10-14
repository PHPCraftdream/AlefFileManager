// SPDX-License-Identifier: MIT OR Apache-2.0
//! Private updater storage and verified package persistence.
use super::{document, download::MAX_PACKAGE_BYTES, UpdaterContext};
use alef_core::{AlefError, ErrorCode};
use alef_package::{Package, Signer};
use std::{
    fs::{self, File, OpenOptions},
    io::Write,
    path::{Path, PathBuf},
    sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    },
};
use tempfile::NamedTempFile;

/// Holds the process-wide update lock for the complete download and verification lifetime.
pub(super) struct Storage {
    updates: PathBuf,
    _lock_file: File,
}

impl Storage {
    pub(super) fn prepare(data: &Path) -> Result<Self, AlefError> {
        ensure_safe_dir(data)?;
        let updates = data.join("updates");
        ensure_safe_dir(&updates)?;
        #[cfg(windows)]
        apply_private_acl(&updates, true)?;
        let lock = updates.join(".alef-update.lock");
        let mut options = OpenOptions::new();
        options.read(true).write(true).create(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let lock_file = options.open(&lock).map_err(io_error)?;
        let meta = fs::symlink_metadata(&lock).map_err(io_error)?;
        if !meta.is_file() || meta.file_type().is_symlink() || is_reparse(&meta) {
            return Err(io_error(std::io::Error::other("unsafe updater lock")));
        }
        lock_file
            .try_lock()
            .map_err(|_| AlefError::new(ErrorCode::UpdateBusy, "another process is updating"))?;
        Ok(Self {
            updates,
            _lock_file: lock_file,
        })
    }

    pub(super) fn temp(&self) -> Result<NamedTempFile, AlefError> {
        let temp = NamedTempFile::new_in(&self.updates).map_err(io_error)?;
        let meta = fs::symlink_metadata(temp.path()).map_err(io_error)?;
        if !meta.is_file() || meta.file_type().is_symlink() || is_reparse(&meta) {
            return Err(io_error(std::io::Error::other(
                "unsafe updater temporary file",
            )));
        }
        Ok(temp)
    }
}

fn io_error(_: std::io::Error) -> AlefError {
    AlefError::new(ErrorCode::UpdateIo, "updater storage operation failed")
}

fn ensure_safe_dir(path: &Path) -> Result<(), AlefError> {
    let absolute = if path.is_absolute() {
        path.to_path_buf()
    } else {
        std::env::current_dir().map_err(io_error)?.join(path)
    };
    let mut current = PathBuf::new();
    for component in absolute.components() {
        if component == std::path::Component::RootDir {
            current.push(component);
            continue;
        }
        current.push(component);
        match fs::symlink_metadata(&current) {
            Ok(meta) if meta.file_type().is_symlink() || is_reparse(&meta) => {
                return Err(io_error(std::io::Error::other("link")));
            }
            Ok(meta) if !meta.is_dir() => {
                return Err(io_error(std::io::Error::other("not directory")));
            }
            Ok(_) => (),
            Err(e) if e.kind() == std::io::ErrorKind::NotFound => {
                match create_private_dir(&current) {
                    Ok(()) => (),
                    Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => (),
                    Err(e) => return Err(io_error(e)),
                }
                let meta = fs::symlink_metadata(&current).map_err(io_error)?;
                if !meta.is_dir() || meta.file_type().is_symlink() || is_reparse(&meta) {
                    return Err(io_error(std::io::Error::other("unsafe directory")));
                }
                #[cfg(windows)]
                apply_private_acl(&current, true)?;
            }
            Err(e) => return Err(io_error(e)),
        }
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(&absolute, fs::Permissions::from_mode(0o700)).map_err(io_error)?;
    }
    Ok(())
}

#[cfg(unix)]
fn create_private_dir(path: &Path) -> std::io::Result<()> {
    use std::os::unix::fs::DirBuilderExt;
    let mut builder = fs::DirBuilder::new();
    builder.mode(0o700).create(path)
}
#[cfg(not(unix))]
fn create_private_dir(path: &Path) -> std::io::Result<()> {
    fs::create_dir(path)
}

#[cfg(windows)]
fn apply_private_acl(path: &Path, inherit: bool) -> Result<(), AlefError> {
    use std::{os::windows::ffi::OsStrExt, ptr};
    use windows_sys::Win32::{
        Foundation::LocalFree,
        Security::{
            Authorization::{
                ConvertStringSecurityDescriptorToSecurityDescriptorW, SDDL_REVISION_1,
            },
            SetFileSecurityW, DACL_SECURITY_INFORMATION, PROTECTED_DACL_SECURITY_INFORMATION,
            PSECURITY_DESCRIPTOR,
        },
    };
    let current = crate::desktop::app::updater_owner_sids().map_err(io_error)?;
    let sddl = if inherit {
        format!("D:P(A;OICI;FA;;;{})(A;OICI;FA;;;SY)", current[0])
    } else {
        format!("D:P(A;;FA;;;{})(A;;FA;;;SY)", current[0])
    };
    let descriptor_text: Vec<u16> = sddl.encode_utf16().chain(Some(0)).collect();
    let mut descriptor: PSECURITY_DESCRIPTOR = ptr::null_mut();
    // SAFETY: the input is terminated UTF-16 and the allocated descriptor is freed below.
    if unsafe {
        ConvertStringSecurityDescriptorToSecurityDescriptorW(
            descriptor_text.as_ptr(),
            SDDL_REVISION_1,
            &mut descriptor,
            ptr::null_mut(),
        )
    } == 0
    {
        return Err(io_error(std::io::Error::last_os_error()));
    }
    let path_text: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    // SAFETY: path and descriptor remain valid for the call; only the protected DACL is set.
    let result = unsafe {
        SetFileSecurityW(
            path_text.as_ptr(),
            DACL_SECURITY_INFORMATION | PROTECTED_DACL_SECURITY_INFORMATION,
            descriptor,
        )
    };
    // SAFETY: descriptor was allocated by the conversion routine.
    unsafe { LocalFree(descriptor) };
    if result == 0 {
        Err(io_error(std::io::Error::last_os_error()))
    } else {
        Ok(())
    }
}

#[cfg(windows)]
fn is_reparse(meta: &fs::Metadata) -> bool {
    use std::os::windows::fs::MetadataExt;
    meta.file_attributes() & 0x400 != 0
}
#[cfg(not(windows))]
fn is_reparse(_: &fs::Metadata) -> bool {
    false
}

pub(super) async fn verify_and_publish(
    storage: Storage,
    temp: NamedTempFile,
    config: &UpdaterContext,
    offered: &document::Document,
) -> Result<(), AlefError> {
    let config = config.clone();
    let offered = offered.clone();
    let cancelled = Arc::new(AtomicBool::new(false));
    let guard = CancellationGuard {
        cancelled: cancelled.clone(),
        active: true,
    };
    let worker_cancelled = cancelled.clone();
    let result = tokio::task::spawn_blocking(move || {
        verify_publish_blocking(storage, temp, config, offered, worker_cancelled)
    })
    .await
    .map_err(|_| AlefError::new(ErrorCode::Internal, "package verifier failed"))?;
    drop(guard.disarm());
    result
}

struct CancellationGuard {
    cancelled: Arc<AtomicBool>,
    active: bool,
}

impl CancellationGuard {
    fn disarm(mut self) -> Self {
        self.active = false;
        self
    }
}

impl Drop for CancellationGuard {
    fn drop(&mut self) {
        if self.active {
            self.cancelled.store(true, Ordering::Release);
        }
    }
}

fn verify_publish_blocking(
    _storage: Storage,
    mut temp: NamedTempFile,
    config: UpdaterContext,
    offered: document::Document,
    cancelled: Arc<AtomicBool>,
) -> Result<(), AlefError> {
    let mut pkg = Package::open_snapshot(temp.path(), MAX_PACKAGE_BYTES).map_err(|_| {
        AlefError::new(
            ErrorCode::UpdateSignatureInvalid,
            "package verification failed",
        )
    })?;
    match pkg.signer() {
        Signer::Unsigned => {
            return Err(AlefError::new(
                ErrorCode::UpdateSignatureInvalid,
                "package is unsigned",
            ));
        }
        Signer::Signed { key, .. } if key.as_slice() != config.trusted_key => {
            return Err(AlefError::new(
                ErrorCode::UpdateKeyMismatch,
                "package signer does not match trusted key",
            ));
        }
        Signer::Signed { .. } => (),
    }
    let bytes = pkg
        .read_limited("alef.ktav", 1024 * 1024)
        .map_err(|_| AlefError::new(ErrorCode::UpdateBadDocument, "package manifest invalid"))?;
    let text = std::str::from_utf8(&bytes)
        .map_err(|_| AlefError::new(ErrorCode::UpdateBadDocument, "package manifest invalid"))?;
    let manifest = alef_core::security::manifest::Manifest::from_ktav_str(text)
        .map_err(|_| AlefError::new(ErrorCode::UpdateBadDocument, "package manifest invalid"))?;
    if manifest.id != config.app_id {
        return Err(AlefError::new(
            ErrorCode::UpdateIdMismatch,
            "package application id does not match",
        ));
    }
    if manifest.version != offered.version
        || alef_core::security::manifest::compare_versions(&offered.version, &config.version).ok()
            != Some(std::cmp::Ordering::Greater)
    {
        return Err(AlefError::new(
            ErrorCode::UpdateBadDocument,
            "package version does not match offer",
        ));
    }
    let dir = config.data.join("updates");
    let offered_version = alef_core::security::manifest::validate_package_version(&offered.version)
        .map_err(|_| AlefError::new(ErrorCode::UpdateBadDocument, "invalid version"))?;
    let mut remove_after_publish = Vec::new();
    for entry in fs::read_dir(&dir).map_err(io_error)? {
        let entry = entry.map_err(io_error)?;
        let path = entry.path();
        if path.extension().and_then(|x| x.to_str()) != Some("alef") {
            continue;
        }
        let meta = fs::symlink_metadata(&path).map_err(io_error)?;
        if !meta.is_file() || meta.file_type().is_symlink() || is_reparse(&meta) {
            return Err(io_error(std::io::Error::other("unsafe update entry")));
        }
        let Some(stem) = path.file_stem().and_then(|s| s.to_str()) else {
            continue;
        };
        let Ok(existing_version) = alef_core::security::manifest::validate_package_version(stem)
        else {
            continue;
        };
        let mut existing = match Package::open_snapshot(&path, MAX_PACKAGE_BYTES) {
            Ok(p) => p,
            Err(_) => continue,
        };
        let valid = match existing.signer() {
            Signer::Signed { key, .. } => key.as_slice() == config.trusted_key,
            Signer::Unsigned => false,
        };
        if !valid {
            continue;
        }
        let Ok(existing_bytes) = existing.read_limited("alef.ktav", 1024 * 1024) else {
            continue;
        };
        let Ok(existing_text) = std::str::from_utf8(&existing_bytes) else {
            continue;
        };
        let Ok(existing_manifest) =
            alef_core::security::manifest::Manifest::from_ktav_str(existing_text)
        else {
            continue;
        };
        if existing_manifest.id == config.app_id && existing_manifest.version == stem {
            match existing_version.cmp_precedence(&offered_version) {
                std::cmp::Ordering::Greater => {
                    return Err(AlefError::new(
                        ErrorCode::UpdateNotAvailable,
                        "a newer update is already stored",
                    ));
                }
                std::cmp::Ordering::Less => remove_after_publish.push(path),
                std::cmp::Ordering::Equal => {
                    let target = dir.join(format!("{}.alef", offered.version));
                    if path != target {
                        remove_after_publish.push(path);
                    }
                }
            }
        }
    }
    if cancelled.load(Ordering::Acquire) {
        return Err(AlefError::new(
            ErrorCode::Internal,
            "package verification cancelled",
        ));
    }
    temp.as_file_mut().flush().map_err(io_error)?;
    temp.as_file().sync_all().map_err(io_error)?;
    let target = dir.join(format!("{}.alef", offered.version));
    if cancelled.load(Ordering::Acquire) {
        return Err(AlefError::new(
            ErrorCode::Internal,
            "package verification cancelled",
        ));
    }
    temp.persist_noclobber(target.clone()).map_err(|e| {
        if e.error.kind() == std::io::ErrorKind::AlreadyExists {
            AlefError::new(
                ErrorCode::UpdateAlreadyDownloaded,
                "update already downloaded",
            )
        } else {
            io_error(e.error)
        }
    })?;
    for path in remove_after_publish {
        if let Err(prune_error) = fs::remove_file(path) {
            return match fs::remove_file(&target) {
                Ok(()) => Err(io_error(prune_error)),
                Err(rollback_error) => Err(io_error(std::io::Error::other(format!(
                    "update pruning failed: {prune_error}; removing published update failed: {rollback_error}"
                )))),
            };
        }
    }
    Ok(())
}
