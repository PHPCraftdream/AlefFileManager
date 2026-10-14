// SPDX-License-Identifier: MIT OR Apache-2.0
//! Foundation for the installed-application store. The tree is trusted to remain stable
//! during operations; checks reject pre-existing links/reparse points, not hostile rename races.
use self::integration::Integration;
use alef_core::{AlefError, ErrorCode};
use std::{
    fmt,
    future::Future,
    io::Read,
    path::{Path, PathBuf},
    pin::Pin,
    sync::Arc,
};

pub(crate) mod flow;
pub(crate) mod integration;
pub(crate) mod store;
pub(crate) mod uninstall;

#[cfg(test)]
mod tests;

pub const RECORD_LIMIT: u64 = 16 * 1024;

#[derive(Debug)]
pub struct InstallError {
    pub category: &'static str,
    pub detail: String,
    pub exit: u8,
}
impl InstallError {
    pub fn new(category: &'static str, detail: impl Into<String>, exit: u8) -> Self {
        Self {
            category,
            detail: detail.into(),
            exit,
        }
    }
    pub fn exit(&self) -> u8 {
        self.exit
    }
}
impl fmt::Display for InstallError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}: {}", self.category, self.detail)
    }
}
impl std::error::Error for InstallError {}

/// Install a verified package using the runtime's trust prompt and native integration.
pub async fn install(
    package: &std::path::Path,
    trust_key: Option<String>,
    allow_unsigned: bool,
) -> Result<String, InstallError> {
    use crate::{
        application::{source, trust::TrustInputs},
        consent::runtime_home,
    };
    let (prepared, bytes) = source::load_install(package).map_err(|e| {
        InstallError::new(e.category.unwrap_or("PACKAGE_INVALID"), e.detail, e.exit)
    })?;
    let executable =
        std::env::current_exe().map_err(|e| InstallError::new("APP_STORE", e.to_string(), 1))?;
    let id = prepared.manifest.id.clone();
    let home = runtime_home();
    let mut integration = integration::NativeIntegration::new();
    let outcome = flow::install_in(
        &home,
        &executable,
        prepared,
        bytes,
        &TrustInputs {
            trust_key,
            allow_unsigned,
        },
        &crate::application::trust::asker_from_environment()
            .map_err(|e| InstallError::new("INVALID_ARGUMENT", e, 2))?,
        &mut integration,
    )
    .await?;
    Ok(match outcome {
        flow::InstallOutcome::Installed(r) => format!("{} {} installed", id, r.version),
        flow::InstallOutcome::AlreadyInstalled(r) => {
            format!("{} {} already installed", id, r.version)
        }
    })
}

pub fn uninstall(id: &str, purge_data: bool) -> Result<(), InstallError> {
    use crate::{
        application::plan::{application_base_dirs, package_path_vars},
        consent::runtime_home,
    };
    let home = runtime_home();
    let vars =
        package_path_vars(id).map_err(|e| InstallError::new("INVALID_ARGUMENT", e.message, 2))?;
    let bases = application_base_dirs(&vars.home);
    let mut integration = integration::NativeIntegration::new();
    uninstall::uninstall_in(
        &home,
        id,
        purge_data,
        &mut integration,
        &bases,
        &mut |path| {
            println!("removing {}", path.display());
            Ok(())
        },
    )
}

pub fn apps(json: bool) -> Result<String, InstallError> {
    use crate::consent::runtime_home;
    let rows = list_records(&runtime_home().join("apps"))?;
    if json {
        let values: Vec<serde_json::Value> = rows
            .into_iter()
            .map(|row| match row {
                AppListing::Installed(r) => {
                    serde_json::json!({"id":r.id,"version":r.version,"name":r.name})
                }
                AppListing::Damaged { hash } => serde_json::json!({"hash":hash,"status":"DAMAGED"}),
            })
            .collect();
        return serde_json::to_string(&values)
            .map_err(|e| InstallError::new("APP_STORE", e.to_string(), 1));
    }
    Ok(rows
        .into_iter()
        .map(|row| match row {
            AppListing::Installed(r) => format!(
                "{} {} {}",
                r.id,
                r.version,
                r.name.replace('\n', "\\n").replace('\r', "\\r")
            ),
            AppListing::Damaged { hash } => format!("{hash} DAMAGED"),
        })
        .collect::<Vec<_>>()
        .join("\n"))
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AppRecord {
    pub id: String,
    pub name: String,
    pub version: String,
    pub icon: Option<String>,
    pub package: String,
    /// Diagnostic only. Never execute this path based on persisted metadata.
    pub executable: PathBuf,
    pub installed_time: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AppListing {
    Installed(AppRecord),
    Damaged { hash: String },
}

pub fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || "._-".contains(c))
        && !id.split('.').any(str::is_empty)
}

pub fn read_record(path: &std::path::Path, hash: &str) -> Result<AppRecord, InstallError> {
    if !crate::runtimes::store::is_plain_directory(path) {
        return Err(InstallError::new(
            "APP_RECORD",
            "application directory is not plain",
            1,
        ));
    }
    let record = read_record_file(&path.join("app.json"), hash)?;
    let package = path.join(&record.package);
    if !crate::runtimes::store::is_regular_file(&package) {
        return Err(InstallError::new(
            "APP_RECORD",
            "referenced package is not plain",
            1,
        ));
    }
    Ok(record)
}

fn read_record_file(path: &std::path::Path, hash: &str) -> Result<AppRecord, InstallError> {
    if hash.len() != 64
        || !hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(InstallError::new(
            "INVALID_ARGUMENT",
            "invalid application hash",
            2,
        ));
    }
    if !crate::runtimes::store::is_regular_file(path) {
        return Err(InstallError::new(
            "APP_STORE",
            "record is not a plain file",
            1,
        ));
    }
    let mut file =
        std::fs::File::open(path).map_err(|e| InstallError::new("APP_STORE", e.to_string(), 1))?;
    let mut bytes = Vec::new();
    file.by_ref()
        .take(RECORD_LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(|e| InstallError::new("APP_STORE", e.to_string(), 1))?;
    if bytes.len() as u64 > RECORD_LIMIT {
        return Err(InstallError::new(
            "APP_RECORD",
            "record exceeds size limit",
            1,
        ));
    }
    let record: AppRecord = serde_json::from_slice(&bytes)
        .map_err(|e| InstallError::new("APP_RECORD", e.to_string(), 1))?;
    // The runtime store's parser enforces the same strict SemVer 2.0.0 contract used
    // for runtime directory names; validate before deriving any persisted path.
    if alef_core::security::manifest::validate_package_version(&record.version).is_err()
        || !valid_id(&record.id)
        || record.id_hash() != hash
        || record.package != format!("{}.alef", record.version)
        || record
            .icon
            .as_deref()
            .is_some_and(|icon| icon != "icon.png")
        || !record.executable.is_absolute()
    {
        return Err(InstallError::new(
            "APP_RECORD",
            "record failed semantic validation",
            1,
        ));
    }
    Ok(record)
}

impl AppRecord {
    fn id_hash(&self) -> String {
        format!("{:x}", sha2::Sha256::digest(self.id.as_bytes()))
    }
}
use sha2::Digest;

pub fn list_records(root: &std::path::Path) -> Result<Vec<AppListing>, InstallError> {
    let root_metadata = match std::fs::symlink_metadata(root) {
        Ok(metadata) if crate::runtimes::store::is_plain_directory(root) => Some(metadata),
        Ok(_) => {
            return Err(InstallError::new(
                "APP_STORE",
                "application store root is not plain",
                1,
            ));
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => None,
        Err(e) => return Err(InstallError::new("APP_STORE", e.to_string(), 1)),
    };
    if root_metadata.is_none() {
        return Ok(Vec::new());
    }
    let mut rows = Vec::new();
    let entries = match std::fs::read_dir(root) {
        Ok(entries) => entries,
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => return Ok(rows),
        Err(e) => return Err(InstallError::new("APP_STORE", e.to_string(), 1)),
    };
    for entry in entries {
        let entry = entry.map_err(|e| InstallError::new("APP_STORE", e.to_string(), 1))?;
        let hash = entry.file_name().to_string_lossy().into_owned();
        if hash.starts_with('.')
            || hash.len() != 64
            || !hash
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            continue;
        }
        rows.push(match read_record(&entry.path(), &hash) {
            Ok(record) => AppListing::Installed(record),
            Err(_) => AppListing::Damaged { hash },
        });
    }
    rows.sort_by(|a, b| key(a).cmp(key(b)));
    Ok(rows)
}
fn key(row: &AppListing) -> &str {
    match row {
        AppListing::Installed(r) => &r.id,
        AppListing::Damaged { hash } => hash,
    }
}

/// The `updater.install` callback the launcher provides: the same transactional install as
/// `alef install`, pinned to the running package's signer, and the restart into the new version.
pub struct StoreApplier {
    home: PathBuf,
    executable: PathBuf,
    /// The fingerprint of the pinned signer of the running application; `apply` accepts only it.
    fingerprint: String,
    integration: Arc<tokio::sync::Mutex<Box<dyn Integration>>>,
    termination: alef_modules::Termination,
}

impl StoreApplier {
    /// The launcher default: the native integration pinned to the running signer.
    pub fn native(
        home: PathBuf,
        executable: PathBuf,
        fingerprint: String,
        termination: alef_modules::Termination,
    ) -> Self {
        Self {
            home,
            executable,
            fingerprint,
            integration: Arc::new(tokio::sync::Mutex::new(Box::new(
                integration::NativeIntegration::new(),
            ))),
            termination,
        }
    }

    /// A custom integration, for tests and alternative backends.
    pub fn new(
        home: PathBuf,
        executable: PathBuf,
        fingerprint: String,
        integration: Box<dyn Integration>,
        termination: alef_modules::Termination,
    ) -> Self {
        Self {
            home,
            executable,
            fingerprint,
            integration: Arc::new(tokio::sync::Mutex::new(integration)),
            termination,
        }
    }

    fn load_error(error: crate::application::source::LoadFailure) -> AlefError {
        let category = error.category.unwrap_or("PACKAGE_INVALID");
        let code = if category.starts_with("PACKAGE_SIGNATURE") || category.starts_with("SIGNATURE")
        {
            ErrorCode::UpdateSignatureInvalid
        } else {
            ErrorCode::UpdateIo
        };
        AlefError::new(code, format!("{}: {}", category, error.detail))
    }

    fn install_error(error: InstallError) -> AlefError {
        // Nothing was changed: the trust refusals happen before any artifact is published.
        let code = match error.category {
            "SIGNER_CHANGED" | "SIGNATURE_REQUIRED" | "TRUST_REFUSED" => {
                ErrorCode::UpdateKeyMismatch
            }
            "BUSY" => ErrorCode::UpdateBusy,
            _ => ErrorCode::UpdateIo,
        };
        AlefError::new(code, format!("{}: {}", error.category, error.detail))
    }
}

impl alef_modules::net::updater::UpdateApplier for StoreApplier {
    fn apply<'a>(
        &'a self,
        package: &'a Path,
    ) -> Pin<Box<dyn Future<Output = Result<PathBuf, AlefError>> + Send + 'a>> {
        Box::pin(async move {
            // load_install hashes and verifies the archive: keep it off the async workers.
            let source = package.to_owned();
            let (prepared, bytes) = tokio::task::spawn_blocking(move || {
                crate::application::source::load_install(&source)
            })
            .await
            .map_err(|e| AlefError::new(ErrorCode::UpdateIo, e.to_string()))?
            .map_err(Self::load_error)?;
            if prepared.fingerprint() != Some(self.fingerprint.as_str()) {
                return Err(AlefError::new(
                    ErrorCode::UpdateKeyMismatch,
                    "the update is not signed by the pinned key",
                ));
            }
            // The signer was already checked against the pin above, so `decide_with_asker`
            // accepts the existing pin and never opens the ask window; an unsigned package is
            // refused as SIGNATURE_REQUIRED, again without a prompt.
            let mut integration = self.integration.lock().await;
            let outcome = flow::install_in(
                &self.home,
                &self.executable,
                prepared,
                bytes,
                &crate::application::trust::TrustInputs {
                    trust_key: Some(self.fingerprint.clone()),
                    allow_unsigned: false,
                },
                &crate::application::trust::TrustAsker::Window,
                &mut **integration,
            )
            .await
            .map_err(Self::install_error)?;
            Ok(match outcome {
                flow::InstallOutcome::Installed(record)
                | flow::InstallOutcome::AlreadyInstalled(record) => self
                    .home
                    .join("apps")
                    .join(format!("{:x}", sha2::Sha256::digest(record.id.as_bytes())))
                    .join(&record.package),
            })
        })
    }

    fn relaunch<'a>(
        &'a self,
        package: &'a Path,
    ) -> Pin<Box<dyn Future<Output = Result<(), AlefError>> + Send + 'a>> {
        let program = std::env::current_exe().map_err(|e| {
            AlefError::new(
                ErrorCode::UpdateRelaunchFailed,
                format!("resolve current executable: {e}"),
            )
        });
        Box::pin(async move {
            let program = program?;
            // The child inherits the launcher environment; `run` reopens the installed package.
            let arguments = [
                std::ffi::OsString::from("run"),
                package.as_os_str().to_owned(),
            ];
            let extra = [(
                crate::relaunch::REPLACES_VARIABLE.to_owned(),
                std::process::id().to_string(),
            )];
            crate::relaunch::detached_with(&program, &arguments, &extra).map_err(|e| {
                AlefError::new(
                    ErrorCode::UpdateRelaunchFailed,
                    format!("start the new version: {e}"),
                )
            })?;
            // The child waits for this process to exit before it boots, so the single-instance
            // endpoint of the old version is never seen by the new one. The child handle is
            // simply dropped without waiting: the process keeps running and outlives this one.
            // `Termination::request` is the same `before-quit` round `app.exit`/`app.quit` gives
            // the documents; after it the bridge quits the process and the promise never resolves.
            if !self.termination.request(0).await {
                return Err(AlefError::new(
                    ErrorCode::UpdateRelaunchFailed,
                    "the running application refused to quit for the update",
                ));
            }
            Ok(())
        })
    }
}

#[cfg(test)]
#[path = "record_tests.rs"]
mod record_tests;
