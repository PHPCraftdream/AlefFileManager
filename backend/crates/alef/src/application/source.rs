// SPDX-License-Identifier: MIT OR Apache-2.0
//! One launcher pipeline, two application sources: a directory (development, exactly as today)
//! or a package file verified before anything is trusted.
use std::{
    fmt,
    io::{Cursor, Read},
    path::{Path, PathBuf},
};

use alef_core::security::{
    authority::Authority,
    consent::Identity,
    manifest::{runtime_requirement_met, validate_package_version, Manifest},
    sidecar::is_sidecar_reference,
};
use alef_package::PackageError;
use sha2::{Digest, Sha256};

use crate::plan::MANIFEST_FILE;

/// An archive larger than this is refused before its bytes are read.
pub const ARCHIVE_LIMIT: u64 = 256 * 1024 * 1024;
/// Any single package entry larger than this is refused before it is read.
pub const ASSET_LIMIT: u64 = 64 * 1024 * 1024;
/// The manifest is metadata: a much tighter bound than an asset.
pub const MANIFEST_LIMIT: u64 = 1024 * 1024;
/// Icons are bounded tighter than assets; they are decoded eagerly by the runtime.
pub const ICON_LIMIT: u64 = 4 * 1024 * 1024;
/// An icon larger than this in either dimension is refused before it is decoded.
pub const ICON_DIMENSION_LIMIT: u32 = 1024;

/// Why a source cannot be loaded: a stable category, its detail, and the exit code.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LoadFailure {
    /// `None` for operational I/O failures, which print without a category.
    pub category: Option<&'static str>,
    pub detail: String,
    pub exit: u8,
    /// The unmet runtime requirement, when the category is `RUNTIME_INCOMPATIBLE`.
    pub runtime_requirement: Option<String>,
}

impl fmt::Display for LoadFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self.category {
            Some(category) => write!(f, "{category}: {}", self.detail),
            None => f.write_str(&self.detail),
        }
    }
}

/// An application source: today's directory or a fully verified package.
pub enum ApplicationSource {
    Directory { dir: PathBuf },
    Package(Box<Prepared>),
}

/// A package verified end to end before any trust or consent write.
pub struct Prepared {
    pub path: PathBuf,
    pub manifest: Manifest,
    pub signer: alef_package::Signer,
    pub authority: Authority,
    /// SHA-256 of the exact snapshot bytes, 64 lowercase hex.
    pub archive_digest: String,
    /// Root `icon.png` of the package; `None` falls back to the runtime icon.
    pub icon_png: Option<Vec<u8>>,
    /// Hex of the raw signing key of the package; `None` when it is unsigned.
    pub public_key: Option<String>,
    pub package: alef_package::Package<Cursor<Vec<u8>>>,
    pub selected_root: String,
}

impl Prepared {
    /// The signer of the consent identity: `pkg-signed:<fingerprint>` or `pkg-unsigned:<digest>`.
    pub fn identity(&self) -> Identity {
        Identity {
            app_id: self.manifest.id.clone(),
            signer: Some(match &self.signer {
                alef_package::Signer::Signed { fingerprint, .. } => {
                    format!("pkg-signed:{fingerprint}")
                }
                alef_package::Signer::Unsigned => {
                    format!("pkg-unsigned:{}", self.archive_digest)
                }
            }),
        }
    }

    /// The fingerprint of the package signer, `None` when it is unsigned.
    pub fn fingerprint(&self) -> Option<&str> {
        match &self.signer {
            alef_package::Signer::Signed { fingerprint, .. } => Some(fingerprint),
            alef_package::Signer::Unsigned => None,
        }
    }

    /// The raw public key of the package signer as hex, `None` when it is unsigned.
    pub fn public_key_hex(&self) -> Option<&str> {
        self.public_key.as_deref()
    }
}

/// The bytes as lowercase hex, two characters per byte.
pub(crate) fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

/// Loads the application source at `path`: a directory exactly as today, otherwise a package.
pub fn load(path: &Path) -> Result<ApplicationSource, LoadFailure> {
    load_with_runtime_check(path, true)
}

/// Loads and fully verifies application metadata without requiring this runtime to run it.
pub fn load_metadata(path: &Path) -> Result<ApplicationSource, LoadFailure> {
    load_with_runtime_check(path, false)
}

fn load_with_runtime_check(
    path: &Path,
    check_runtime: bool,
) -> Result<ApplicationSource, LoadFailure> {
    let meta = std::fs::metadata(path).map_err(|error| LoadFailure {
        category: Some("NOT_A_PACKAGE"),
        detail: format!(
            "{}: expected a format-1 Alef ZIP package: {error}",
            path.display()
        ),
        exit: 2,
        runtime_requirement: None,
    })?;
    if meta.is_dir() {
        return Ok(ApplicationSource::Directory {
            dir: path.to_owned(),
        });
    }
    if meta.is_file() {
        return Ok(ApplicationSource::Package(Box::new(prepare(
            path,
            check_runtime,
        )?)));
    }
    Err(not_a_package(path))
}

pub fn load_install(path: &Path) -> Result<(Prepared, Vec<u8>), LoadFailure> {
    load_install_in(path, &crate::runtimes::store::root())
}

/// [`load_install`] against an explicit runtime-store root, so callers that own a home
/// (the applier, tests) never depend on the process environment.
pub fn load_install_in(
    path: &Path,
    runtime_store: &Path,
) -> Result<(Prepared, Vec<u8>), LoadFailure> {
    let meta = match std::fs::metadata(path) {
        Ok(meta) => meta,
        Err(_) => return Err(not_a_package(path)),
    };
    if meta.is_dir() {
        return Err(failure(
            "INVALID_ARGUMENT",
            "application installation requires a package file",
        ));
    }
    if !meta.is_file() {
        return Err(not_a_package(path));
    }
    prepare_snapshot(path, true, runtime_store, true)
        .map(|(prepared, bytes)| (prepared, bytes.expect("retained installation snapshot")))
}

fn prepare(path: &Path, check_runtime: bool) -> Result<Prepared, LoadFailure> {
    prepare_snapshot(path, false, Path::new(""), check_runtime).map(|(prepared, _)| prepared)
}

/// Freezes one bounded source snapshot; the install mode (used by `alef install` and the updater
/// applier) retains those exact verified bytes and also accepts a range satisfied by an installed
/// runtime version (the install-mode compatibility check lists the versions installed in
/// `runtime_store`); the run mode stays strict so the launcher's relaunch (M7a.4) picks the right
/// binary.
fn prepare_snapshot(
    path: &Path,
    install: bool,
    runtime_store: &Path,
    check_runtime: bool,
) -> Result<(Prepared, Option<Vec<u8>>), LoadFailure> {
    let file = std::fs::File::open(path).map_err(io_failure)?;
    let meta = file.metadata().map_err(io_failure)?;
    if !meta.is_file() {
        return Err(not_a_package(path));
    }
    if meta.len() > ARCHIVE_LIMIT {
        return Err(run_limit(format!(
            "the archive exceeds {ARCHIVE_LIMIT} bytes"
        )));
    }
    let mut bytes = Vec::new();
    if install {
        bytes
            .try_reserve_exact(usize::try_from(meta.len()).unwrap_or(usize::MAX))
            .map_err(|_| run_limit("cannot allocate the bounded installation snapshot"))?;
    }
    file.take(ARCHIVE_LIMIT + 1)
        .read_to_end(&mut bytes)
        .map_err(io_failure)?;
    if bytes.len() as u64 > ARCHIVE_LIMIT {
        return Err(run_limit(format!(
            "the archive exceeds {ARCHIVE_LIMIT} bytes"
        )));
    }
    let archive_digest = format!("{:x}", Sha256::digest(&bytes));
    let retained_bytes = if install {
        let mut snapshot = Vec::new();
        snapshot
            .try_reserve_exact(bytes.len())
            .map_err(|_| run_limit("cannot allocate the bounded installation snapshot"))?;
        snapshot.extend_from_slice(&bytes);
        Some(snapshot)
    } else {
        None
    };
    // `NOT_A_PACKAGE` is for input that is not a ZIP at all; a recognizable ZIP that fails
    // the strict profile stays a format failure.
    if !bytes.starts_with(b"PK\x03\x04") && !bytes.starts_with(b"PK\x05\x06") {
        return Err(not_a_package(path));
    }
    let mut package =
        alef_package::Package::from_bytes(bytes).map_err(|error| package_failure(&error))?;

    // The manifest is read through the verified package only, within its own limit.
    let manifest_bytes = match package.read_limited(MANIFEST_FILE, MANIFEST_LIMIT) {
        Ok(bytes) => bytes,
        Err(PackageError::EntryNotFound(_)) => {
            return Err(manifest_failure("missing root alef.ktav"))
        }
        Err(error) => return Err(package_failure(&error)),
    };
    let text = std::str::from_utf8(&manifest_bytes)
        .map_err(|_| manifest_failure("alef.ktav is not UTF-8"))?;
    let manifest = Manifest::from_ktav_str(text)
        .map_err(|error| manifest_failure(format!("{}: {}", error.code.as_str(), error.message)))?;
    if let Err(error) = validate_package_version(&manifest.version) {
        return Err(manifest_failure(format!(
            "version: {:?} is not strict SemVer 2.0.0: {error}",
            manifest.version
        )));
    }
    if check_runtime {
        if let Some(requirement) = manifest.runtime.as_deref() {
            // Install mode (alef install / the updater applier) also accepts an already installed
            // runtime version; the relaunch after the update then selects it (M7a.4). The run
            // mode stays strict so the launcher, not this loader, handles the mismatch.
            let satisfied = runtime_requirement_met(requirement, env!("CARGO_PKG_VERSION"))
                || (install
                    && crate::runtimes::store::installed_in(runtime_store)
                        .map(|installed| {
                            installed
                                .iter()
                                .any(|entry| runtime_requirement_met(requirement, &entry.version))
                        })
                        .unwrap_or(false));
            if !satisfied {
                return Err(LoadFailure {
                    category: Some("RUNTIME_INCOMPATIBLE"),
                    detail: crate::runtimes::command::incompatible_detail(requirement),
                    exit: 2,
                    runtime_requirement: Some(requirement.to_owned()),
                });
            }
        }
    }

    // Sidecars are directory concepts; refuse them before any trust is established.
    for reference in manifest
        .permissions
        .cli
        .exec
        .iter()
        .map(|program| program.as_str())
        .chain(
            manifest
                .permissions
                .cli
                .commands
                .iter()
                .map(|command| command.program.as_str()),
        )
    {
        if is_sidecar_reference(reference) {
            return Err(manifest_failure(
                "sidecar: is unavailable for archive applications",
            ));
        }
    }

    // The selected root must really have files; an exact prefix, so `dist2` is not `dist`.
    let selected_root = manifest.assets.clone().unwrap_or_else(|| "dist".to_owned());
    let prefix = format!("{selected_root}/");
    if !package.names().any(|name| name.starts_with(&prefix)) {
        return Err(manifest_failure(format!(
            "assets: {selected_root:?} has no files in the package"
        )));
    }

    let icon_png = match package.read_limited("icon.png", ICON_LIMIT) {
        Ok(bytes) => {
            check_png_dimensions(&bytes)?;
            Some(bytes)
        }
        Err(PackageError::EntryNotFound(_)) => None,
        Err(error) => return Err(package_failure(&error)),
    };

    let signer = package.signer().clone();
    let public_key = match &signer {
        alef_package::Signer::Signed { key, .. } => Some(hex(key)),
        alef_package::Signer::Unsigned => None,
    };
    let authority = match &signer {
        alef_package::Signer::Signed { fingerprint, .. } => Authority::SignedPackage {
            fingerprint: fingerprint.clone(),
        },
        alef_package::Signer::Unsigned => Authority::UnsignedPackage {
            archive_digest: archive_digest.clone(),
        },
    };
    Ok((
        Prepared {
            path: path.to_owned(),
            manifest,
            signer,
            authority,
            archive_digest,
            icon_png,
            public_key,
            package,
            selected_root,
        },
        retained_bytes,
    ))
}

/// Bounds the PNG dimensions before anything decodes or allocates for the image.
/// Non-PNG bytes pass here; the runtime reports the decode failure when it decodes.
fn check_png_dimensions(bytes: &[u8]) -> Result<(), LoadFailure> {
    const SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
    if bytes.len() < 24 || bytes[..8] != SIGNATURE || &bytes[12..16] != b"IHDR" {
        return Ok(());
    }
    let width = u32::from_be_bytes(bytes[16..20].try_into().expect("IHDR width"));
    let height = u32::from_be_bytes(bytes[20..24].try_into().expect("IHDR height"));
    if width > ICON_DIMENSION_LIMIT || height > ICON_DIMENSION_LIMIT {
        return Err(run_limit(format!(
            "icon.png is {width}x{height}, at most {ICON_DIMENSION_LIMIT}x{ICON_DIMENSION_LIMIT}"
        )));
    }
    Ok(())
}

/// Maps every `PackageError` to a stable category structurally; no text parsing.
fn package_failure(error: &PackageError) -> LoadFailure {
    use PackageError as Error;
    match error {
        Error::Io(io) => LoadFailure {
            category: None,
            detail: format!("cannot read the package: {io}"),
            exit: 1,
            runtime_requirement: None,
        },
        Error::NotRegularFile => failure("NOT_A_PACKAGE", "expected a format-1 Alef ZIP package"),
        Error::MissingManifest => failure("PACKAGE_MANIFEST", "missing root alef.ktav"),
        Error::CrcMismatch => failure("PACKAGE_INTEGRITY", "CRC-32 mismatch"),
        Error::HashMismatch => failure("PACKAGE_INTEGRITY", "SHA-256 mismatch"),
        Error::MissingHash(_)
        | Error::ExtraHash(_)
        | Error::InvalidHashList(_)
        | Error::MissingHashes => failure("PACKAGE_INTEGRITY", format!("{error}")),
        Error::InvalidSignature(_) | Error::InvalidSigningKey | Error::SignatureMismatch => {
            failure("PACKAGE_SIGNATURE", format!("{error}"))
        }
        Error::SnapshotSizeLimit(_)
        | Error::SnapshotLengthChanged
        | Error::ArchiveSizeLimit
        | Error::CentralSizeLimit
        | Error::EntryCountLimit
        | Error::TotalSizeLimit
        | Error::EntrySizeLimit(_)
        | Error::MetadataSizeLimit(_, _)
        | Error::ReadSizeLimit(_) => failure("PACKAGE_RUN_LIMIT", format!("{error}")),
        Error::Entry(name, inner) => {
            let mut mapped = package_failure(inner);
            mapped.detail = format!("{name}: {}", mapped.detail);
            mapped
        }
        // `PackageError` is `#[non_exhaustive]`: every remaining variant, and any future
        // one, is a format failure of the file the user handed over.
        _ => failure("PACKAGE_FORMAT", format!("{error}")),
    }
}

fn failure(category: &'static str, detail: impl Into<String>) -> LoadFailure {
    LoadFailure {
        category: Some(category),
        detail: detail.into(),
        exit: 2,
        runtime_requirement: None,
    }
}

fn manifest_failure(detail: impl Into<String>) -> LoadFailure {
    failure("MANIFEST_INVALID", detail)
}

fn run_limit(detail: impl Into<String>) -> LoadFailure {
    failure("PACKAGE_RUN_LIMIT", detail)
}

fn not_a_package(path: &Path) -> LoadFailure {
    failure(
        "NOT_A_PACKAGE",
        format!("{}: expected a format-1 Alef ZIP package", path.display()),
    )
}

fn io_failure(error: std::io::Error) -> LoadFailure {
    LoadFailure {
        category: None,
        detail: format!("cannot read the package: {error}"),
        exit: 1,
        runtime_requirement: None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::application::support;

    fn load_bytes(dir: &Path, bytes: &[u8]) -> Result<ApplicationSource, LoadFailure> {
        let path = dir.join("app.alef");
        std::fs::write(&path, bytes).expect("write the package");
        load(&path)
    }

    fn default_files() -> Vec<(&'static str, Vec<u8>)> {
        vec![
            ("alef.ktav", support::manifest_text("").into_bytes()),
            ("dist/index.html", b"<html></html>".to_vec()),
        ]
    }

    fn referenced<'a>(files: &'a [(&'static str, Vec<u8>)]) -> Vec<(&'static str, &'a [u8])> {
        files
            .iter()
            .map(|(name, bytes)| (*name, &bytes[..]))
            .collect()
    }

    fn prepare_of(files: &[(&str, &[u8])]) -> Prepared {
        let dir = tempfile::tempdir().expect("tempdir");
        match load_bytes(dir.path(), &support::zip(files)).expect("a valid package") {
            ApplicationSource::Package(prepared) => *prepared,
            ApplicationSource::Directory { .. } => panic!("expected a package"),
        }
    }

    fn category_of(result: Result<ApplicationSource, LoadFailure>) -> LoadFailure {
        match result {
            Ok(_) => panic!("expected a refusal"),
            Err(failure) => failure,
        }
    }

    #[test]
    fn a_directory_is_loaded_as_today_and_a_missing_or_special_path_is_not_a_package() {
        let dir = tempfile::tempdir().expect("tempdir");
        std::fs::write(dir.path().join(MANIFEST_FILE), support::manifest_text(""))
            .expect("write the manifest");
        match load(dir.path()).expect("a directory") {
            ApplicationSource::Directory { dir } => assert!(dir.is_dir()),
            ApplicationSource::Package(_) => panic!("expected a directory"),
        }
        let missing = dir.path().join("missing.alef");
        let failure = category_of(load(&missing));
        assert_eq!(failure.category, Some("NOT_A_PACKAGE"));
        assert_eq!(failure.exit, 2);
        assert!(failure.detail.contains("format-1"));
    }

    #[test]
    fn a_plain_file_that_is_not_a_zip_is_not_a_package() {
        let dir = tempfile::tempdir().expect("tempdir");
        let failure = category_of(load_bytes(dir.path(), b"hello"));
        assert_eq!(failure.category, Some("NOT_A_PACKAGE"));
        assert_eq!(failure.exit, 2);
    }

    #[test]
    fn a_recognizable_zip_that_breaks_the_strict_profile_is_bad_format_not_unsigned() {
        let dir = tempfile::tempdir().expect("tempdir");
        let mut bytes = support::zip(&referenced(&default_files()));
        bytes.truncate(bytes.len() - 10);
        let failure = category_of(load_bytes(dir.path(), &bytes));
        assert_eq!(failure.category, Some("PACKAGE_FORMAT"));
        assert_eq!(failure.exit, 2);
    }

    #[test]
    fn a_crc_or_hash_mismatch_is_integrity_and_never_silently_accepted() {
        // (a) A flipped payload byte fails the CRC-32 of the entry, named by the error.
        let dir = tempfile::tempdir().expect("tempdir");
        let mut bytes = support::zip(&[
            ("alef.ktav", support::manifest_text("").as_bytes()),
            ("dist/index.html", b"<html>indexOf marker</html>"),
        ]);
        let marker = b"indexOf";
        let at = bytes
            .windows(marker.len())
            .position(|window| window == marker)
            .expect("the payload in the archive");
        bytes[at] ^= 0xFF;
        let failure = category_of(load_bytes(dir.path(), &bytes));
        assert_eq!(failure.category, Some("PACKAGE_INTEGRITY"));
        assert!(failure.detail.contains("CRC-32"));
        assert!(failure.detail.contains("index.html"));

        // (b) A payload that differs from its signed hash list fails the SHA-256 check.
        let manifest = support::manifest_text("").into_bytes();
        let list = support::hashes(&[
            ("alef.ktav", &manifest),
            ("dist/index.html", b"<html></html>"),
        ]);
        // The signature is valid over the list; the payload itself is what differs.
        let (sig, _) = support::signature(&list);
        let bytes = support::zip(&[
            ("alef.ktav", &manifest),
            ("dist/index.html", b"<html>mutated</html>"),
            ("META-INF/hashes", &list),
            ("META-INF/signature", &sig),
        ]);
        let failure = category_of(load_bytes(dir.path(), &bytes));
        assert_eq!(failure.category, Some("PACKAGE_INTEGRITY"));
        assert!(failure.detail.contains("SHA-256 mismatch"));
        assert!(failure.detail.contains("index.html"));
    }

    #[test]
    fn a_bad_signature_is_a_signature_failure_and_an_unsigned_package_is_loaded_without_one() {
        // (a) A signature over other bytes fails strict verification.
        let dir = tempfile::tempdir().expect("tempdir");
        let manifest = support::manifest_text("").into_bytes();
        let list = support::hashes(&[
            ("alef.ktav", &manifest),
            ("dist/index.html", b"<html></html>"),
        ]);
        let (sig, _) = support::signature(b"different hashes");
        let bytes = support::zip(&[
            ("alef.ktav", &manifest),
            ("dist/index.html", b"<html></html>"),
            ("META-INF/hashes", &list),
            ("META-INF/signature", &sig),
        ]);
        let failure = category_of(load_bytes(dir.path(), &bytes));
        assert_eq!(failure.category, Some("PACKAGE_SIGNATURE"));
        assert!(failure.detail.contains("strict Ed25519"));

        // (b) An unsigned package loads; its identity is the digest of the exact bytes.
        let files = default_files();
        let bytes = support::zip(&referenced(&files));
        let expected = format!("pkg-unsigned:{:x}", Sha256::digest(&bytes));
        let prepared = prepare_of(&referenced(&files));
        assert_eq!(prepared.fingerprint(), None);
        assert_eq!(
            prepared.identity().signer.as_deref(),
            Some(expected.as_str())
        );

        // (c) Hashes without a signature are still an unsigned package.
        let files = default_files();
        let list = support::hashes(&referenced(&files));
        let mut records = referenced(&files);
        records.push(("META-INF/hashes", &list));
        let prepared = prepare_of(&records);
        assert_eq!(prepared.signer, alef_package::Signer::Unsigned);
    }

    #[test]
    fn a_signed_package_carries_the_hex_of_its_signing_key() {
        let files = default_files();
        let list = support::hashes(&referenced(&files));
        let (sig, fingerprint) = support::signature(&list);
        let mut records = referenced(&files);
        records.push(("META-INF/hashes", &list));
        records.push(("META-INF/signature", &sig));
        let prepared = prepare_of(&records);
        assert_eq!(
            prepared.public_key_hex(),
            Some(support::test_key_hex().as_str())
        );
        assert_eq!(prepared.fingerprint(), Some(fingerprint.as_str()));

        let unsigned = prepare_of(&referenced(&default_files()));
        assert_eq!(unsigned.public_key_hex(), None);
    }

    #[test]
    fn the_manifest_is_read_through_the_verified_package_only_within_its_limit() {
        // (a) No manifest at all.
        let failure = category_of(prepare_load(&[("dist/index.html", b"<html></html>")]));
        assert_eq!(failure.category, Some("PACKAGE_MANIFEST"));
        assert_eq!(failure.detail, "missing root alef.ktav");

        // (b) A manifest of 1 MiB + 1 bytes, padded with harmless Ktav comment lines.
        let pad = format!("## {}\n", "pad ".repeat(2_000));
        let padded = support::manifest_text(&pad.repeat(150)).into_bytes();
        let failure = category_of(prepare_load(&[
            ("alef.ktav", &padded),
            ("dist/index.html", b"<html></html>"),
        ]));
        assert_eq!(failure.category, Some("PACKAGE_RUN_LIMIT"));
        assert!(failure.detail.contains("alef.ktav"));

        // (c) A manifest the core rejects.
        let broken = support::manifest_text("").replace("version:: 1.0.0\n", "");
        let failure = category_of(prepare_load(&[
            ("alef.ktav", broken.as_bytes()),
            ("dist/index.html", b"<html></html>"),
        ]));
        assert_eq!(failure.category, Some("MANIFEST_INVALID"));
    }

    fn prepare_load(files: &[(&str, &[u8])]) -> Result<ApplicationSource, LoadFailure> {
        let dir = tempfile::tempdir().expect("tempdir");
        load_bytes(dir.path(), &support::zip(files))
    }

    #[test]
    fn a_package_needs_a_strict_semver_version() {
        for version in ["1.0", ""] {
            let text = support::manifest_text("")
                .replace("version:: 1.0.0", &format!("version:: {version}"));
            let failure = category_of(prepare_load(&[
                ("alef.ktav", text.as_bytes()),
                ("dist/index.html", b"<html></html>"),
            ]));
            assert_eq!(failure.category, Some("MANIFEST_INVALID"));
        }
        for version in ["1.0.0-beta+meta"] {
            let text = support::manifest_text("")
                .replace("version:: 1.0.0", &format!("version:: {version}"));
            let dir = tempfile::tempdir().expect("tempdir");
            assert!(load_bytes(
                dir.path(),
                &support::zip(&[
                    ("alef.ktav", text.as_bytes()),
                    ("dist/index.html", b"<html></html>"),
                ])
            )
            .is_ok());
        }
    }

    #[test]
    fn a_runtime_requirement_must_be_satisfied_by_this_executable() {
        let text = support::manifest_text("runtime: >=999.0\n");
        let failure = category_of(prepare_load(&[
            ("alef.ktav", text.as_bytes()),
            ("dist/index.html", b"<html></html>"),
        ]));
        assert_eq!(failure.category, Some("RUNTIME_INCOMPATIBLE"));
        assert_eq!(failure.exit, 2);
        assert!(failure.detail.starts_with(&format!(
            "requires >=999.0; this executable provides {}; ",
            env!("CARGO_PKG_VERSION")
        )));
        assert_eq!(failure.runtime_requirement, Some(">=999.0".to_owned()));
        for extra in ["runtime: ^0.1\n", ""] {
            let text = support::manifest_text(extra);
            assert!(prepare_load(&[
                ("alef.ktav", text.as_bytes()),
                ("dist/index.html", b"<html></html>"),
            ])
            .is_ok());
        }
    }

    #[test]
    fn sidecar_declarations_are_refused_for_packages() {
        // The builder keeps `exec: []`, so a targeted replacement expresses the refusal.
        let text = support::manifest_text("").replace("exec: []", "exec: [sidecar:tool]");
        let failure = category_of(prepare_load(&[
            ("alef.ktav", text.as_bytes()),
            ("dist/index.html", b"<html></html>"),
        ]));
        assert_eq!(failure.category, Some("MANIFEST_INVALID"));
        assert!(failure.detail.contains("sidecar: is unavailable"));
    }

    #[test]
    fn the_selected_root_must_have_descendants_in_the_package() {
        // (a) `assets: web` with only `dist/...` files.
        let text = support::manifest_text("assets: web\n");
        let failure = category_of(prepare_load(&[
            ("alef.ktav", text.as_bytes()),
            ("dist/index.html", b"<html></html>"),
        ]));
        assert_eq!(failure.category, Some("MANIFEST_INVALID"));
        assert!(failure.detail.contains("assets"));

        // (b) The default `dist` root with a file inside loads.
        assert!(prepare_load(&[
            ("alef.ktav", support::manifest_text("").as_bytes()),
            ("dist/index.html", b"<html></html>"),
        ])
        .is_ok());

        // (c) `dist2/x` is not under `dist/`: the prefix check is exact.
        let failure = category_of(prepare_load(&[
            ("alef.ktav", support::manifest_text("").as_bytes()),
            ("dist2/x", b"data"),
        ]));
        assert_eq!(failure.category, Some("MANIFEST_INVALID"));
    }

    #[test]
    fn the_icon_is_read_from_the_archive_within_bounds_or_defaults() {
        let base = || vec![("alef.ktav", support::manifest_text("").into_bytes())];
        // (a) No icon: `None`.
        let files = base();
        let mut records = referenced(&files);
        records.push(("dist/index.html", b"<html></html>"));
        assert_eq!(prepare_of(&records).icon_png, None);

        // (b) A small icon is delivered byte for byte.
        let icon = support::png(32, 32);
        let mut records = referenced(&files);
        records.push(("dist/index.html", b"<html></html>"));
        records.push(("icon.png", &icon));
        assert_eq!(prepare_of(&records).icon_png, Some(icon.clone()));

        // (c) Oversized dimensions are refused before any decode.
        let icon = support::png(2000, 2000);
        let failure = category_of(prepare_load(&[
            ("alef.ktav", support::manifest_text("").as_bytes()),
            ("dist/index.html", b"<html></html>"),
            ("icon.png", &icon),
        ]));
        assert_eq!(failure.category, Some("PACKAGE_RUN_LIMIT"));
        assert!(failure.detail.contains("1024"));

        // (d) A 5 MiB icon is refused by the entry limit before it is read.
        let mut icon = support::png(10, 10);
        icon.extend(std::iter::repeat_n(0u8, 5 * 1024 * 1024));
        let failure = category_of(prepare_load(&[
            ("alef.ktav", support::manifest_text("").as_bytes()),
            ("dist/index.html", b"<html></html>"),
            ("icon.png", &icon),
        ]));
        assert_eq!(failure.category, Some("PACKAGE_RUN_LIMIT"));
    }

    #[test]
    fn a_signed_package_exposes_its_fingerprint_and_authority() {
        let files = default_files();
        let (bytes, fingerprint) = support::signed_package(&referenced(&files));
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("app.alef");
        std::fs::write(&path, &bytes).expect("write the package");
        let prepared = match load(&path).expect("a signed package") {
            ApplicationSource::Package(prepared) => *prepared,
            ApplicationSource::Directory { .. } => panic!("expected a package"),
        };
        assert_eq!(prepared.fingerprint(), Some(fingerprint.as_str()));
        assert_eq!(
            prepared.authority,
            Authority::SignedPackage {
                fingerprint: fingerprint.clone()
            }
        );
        assert_eq!(
            prepared.identity().signer.as_deref(),
            Some(format!("pkg-signed:{fingerprint}").as_str())
        );
        assert_eq!(prepared.manifest.id, "org.example.app");
        assert_eq!(prepared.manifest.name, "Example");
        assert_eq!(prepared.manifest.version, "1.0.0");
    }

    #[test]
    fn the_snapshot_is_frozen_and_a_replaced_file_does_not_change_it() {
        let files = default_files();
        let bytes = support::zip(&referenced(&files));
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("app.alef");
        std::fs::write(&path, &bytes).expect("write the package");
        let mut prepared = match load(&path).expect("a package") {
            ApplicationSource::Package(prepared) => *prepared,
            ApplicationSource::Directory { .. } => panic!("expected a package"),
        };
        let other = support::zip(&[("dist/index.html", b"replaced")]);
        std::fs::write(&path, &other).expect("replace the package");
        assert_eq!(
            prepared.archive_digest,
            format!("{:x}", Sha256::digest(&bytes))
        );
        assert_eq!(prepared.manifest.id, "org.example.app");
        assert_eq!(
            prepared.package.read("dist/index.html").expect("entry"),
            b"<html></html>".to_vec()
        );
    }

    #[test]
    fn install_load_preserves_snapshot_after_replacement_and_rejects_directories() {
        let files = default_files();
        let bytes = support::zip(&referenced(&files));
        let dir = tempfile::tempdir().expect("tempdir");
        let path = dir.path().join("app.alef");
        std::fs::write(&path, &bytes).expect("write package");
        let (mut prepared, snapshot) = load_install(&path).expect("installation snapshot");
        std::fs::write(&path, b"overwritten").expect("overwrite source");
        assert_eq!(snapshot, bytes);
        assert_eq!(
            prepared
                .package
                .read("dist/index.html")
                .expect("snapshot entry"),
            b"<html></html>".to_vec()
        );
        let failure = load_install(dir.path()).err().expect("directory refusal");
        assert_eq!(failure.category, Some("INVALID_ARGUMENT"));
        assert_eq!(failure.exit, 2);
        let missing = load_install(&dir.path().join("missing.alef"))
            .err()
            .expect("missing package refusal");
        assert_eq!(missing.category, Some("NOT_A_PACKAGE"));
        assert_eq!(missing.exit, 2);
    }
}
