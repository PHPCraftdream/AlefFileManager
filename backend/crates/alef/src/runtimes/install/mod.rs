// SPDX-License-Identifier: MIT OR Apache-2.0
//! Foundation for the installed-application store. The tree is trusted to remain stable
//! during operations; checks reject pre-existing links/reparse points, not hostile rename races.
use std::{fmt, io::Read, path::PathBuf};

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

#[cfg(test)]
#[path = "record_tests.rs"]
mod record_tests;
