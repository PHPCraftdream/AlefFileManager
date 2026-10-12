// SPDX-License-Identifier: MIT OR Apache-2.0
//! The trust of a package signer, pinned per exact application id: a fail-closed store of
//! immutable records the launcher alone writes.
use std::{
    fmt,
    io::{self, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicU64, Ordering},
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

use crate::{application::window, consent::ask::AppSummary, consent::runtime_home};

/// A single record file is bounded so a hostile folder cannot balloon the parse step.
pub const RECORD_LIMIT: u64 = 4 * 1024;
/// The only record format the launcher reads or writes today.
pub const RECORD_VERSION: u32 = 1;

/// `FILE_ATTRIBUTE_REPARSE_POINT`: any link-like Windows object is refused.
const FILE_ATTRIBUTE_REPARSE_POINT: u32 = 0x400;
/// `ERROR_ALREADY_EXISTS`: `MoveFileExW` without a replace flag loses to an existing record.
const ERROR_ALREADY_EXISTS: i32 = 183;

static TEMP_COUNTER: AtomicU64 = AtomicU64::new(0);

/// The folder holding every pin record under the launcher's runtime home.
pub fn trust_folder() -> PathBuf {
    runtime_home().join("trust")
}

/// The record path for one exact id: the file name is the SHA-256 of the id bytes,
/// so the id itself never becomes a path component.
pub fn pin_path(folder: &Path, app_id: &str) -> PathBuf {
    folder.join(format!("{:x}.json", Sha256::digest(app_id.as_bytes())))
}

/// One immutable pin: the key accepted for one exact application id.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Pin {
    pub version: u32,
    pub app_id: String,
    #[serde(rename = "public_key")]
    pub public_key: String,
    pub fingerprint: String,
}

/// Exactly 64 ascii lowercase hex characters.
fn hex64(text: &str) -> bool {
    text.len() == 64
        && text
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
}

/// The SHA-256 hex digest of the raw key bytes, `None` unless the key is hex64.
fn fingerprint_of(public_key: &str) -> Option<String> {
    if !hex64(public_key) {
        return None;
    }
    let bytes = (0..32)
        .map(|i| u8::from_str_radix(&public_key[i * 2..i * 2 + 2], 16).expect("hex64 checked"))
        .collect::<Vec<u8>>();
    Some(format!("{:x}", Sha256::digest(&bytes)))
}

/// Loads the pin for one exact id, fail-closed: anything but a well-formed, self-consistent
/// plain file is an error, and only a missing record means "unpinned".
pub fn load(folder: &Path, app_id: &str) -> io::Result<Option<Pin>> {
    let path = pin_path(folder, app_id);
    let metadata = match std::fs::symlink_metadata(&path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        Err(error) => return Err(error),
    };
    if metadata.file_type().is_symlink() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the trust record is a link",
        ));
    }
    #[cfg(windows)]
    use std::os::windows::fs::MetadataExt as _;
    #[cfg(windows)]
    if metadata.file_attributes() & FILE_ATTRIBUTE_REPARSE_POINT != 0 {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the trust record is a link",
        ));
    }
    if metadata.is_dir() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the trust record is not a file",
        ));
    }
    if metadata.len() > RECORD_LIMIT {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            format!("the trust record exceeds {} bytes", RECORD_LIMIT),
        ));
    }
    let bytes = std::fs::read(&path)?;
    let pin: Pin = serde_json::from_slice(&bytes).map_err(|error| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            format!("the trust record is damaged: {error}"),
        )
    })?;
    if pin.version != RECORD_VERSION {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the trust record version is not supported",
        ));
    }
    if pin.app_id != app_id {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the trust record belongs to another application",
        ));
    }
    if !hex64(&pin.public_key) || !hex64(&pin.fingerprint) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the trust record key or fingerprint is not 64 lowercase hex",
        ));
    }
    if fingerprint_of(&pin.public_key) != Some(pin.fingerprint.clone()) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the trust record fingerprint does not match its key",
        ));
    }
    Ok(Some(pin))
}

/// Publishes the pin for one exact id, never overwriting an existing record.
pub fn pin(folder: &Path, app_id: &str, public_key: &str) -> io::Result<Pin> {
    if !hex64(public_key) {
        return Err(io::Error::new(
            io::ErrorKind::InvalidData,
            "the public key is not 64 lowercase hex",
        ));
    }
    let fingerprint = fingerprint_of(public_key).ok_or_else(|| {
        io::Error::new(
            io::ErrorKind::InvalidData,
            "the public key is not 64 lowercase hex",
        )
    })?;
    let record = Pin {
        version: RECORD_VERSION,
        app_id: app_id.to_owned(),
        public_key: public_key.to_owned(),
        fingerprint,
    };
    let bytes = serde_json::to_vec(&record).map_err(io::Error::other)?;
    std::fs::create_dir_all(folder)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(folder, std::fs::Permissions::from_mode(0o700))?;
    }
    let unique = TEMP_COUNTER.fetch_add(1, Ordering::Relaxed);
    let temp = folder.join(format!(
        ".{}.{}.{}.tmp",
        &record.fingerprint[..8],
        std::process::id(),
        unique
    ));
    {
        let mut file = std::fs::File::create(&temp)?;
        file.write_all(&bytes)?;
        file.sync_all()?;
    }
    let final_path = pin_path(folder, app_id);
    #[cfg(unix)]
    match std::fs::hard_link(&temp, &final_path) {
        Ok(()) => {
            let _ = std::fs::remove_file(&temp);
        }
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
            let _ = std::fs::remove_file(&temp);
            return resolve_race(folder, &record);
        }
        Err(error) => {
            let _ = std::fs::remove_file(&temp);
            return Err(error);
        }
    }
    #[cfg(windows)]
    {
        use std::os::windows::ffi::OsStrExt;
        let temp_wide = wide(&temp);
        let final_wide = wide(&final_path);
        if unsafe {
            windows_sys::Win32::Storage::FileSystem::MoveFileExW(
                temp_wide.as_ptr(),
                final_wide.as_ptr(),
                0,
            )
        } == 0
        {
            let error = io::Error::last_os_error();
            let _ = std::fs::remove_file(&temp);
            if error.raw_os_error() == Some(ERROR_ALREADY_EXISTS) {
                return resolve_race(folder, &record);
            }
            return Err(error);
        }
    }
    #[cfg(not(any(unix, windows)))]
    {
        let _ = std::fs::remove_file(&temp);
        return Err(io::Error::other(
            "trust publication is not supported on this platform",
        ));
    }
    #[cfg(unix)]
    if let Ok(dir) = std::fs::File::open(folder) {
        let _ = dir.sync_all();
    }
    Ok(record)
}

/// Wide (UTF-16, NUL-terminated) path for the Windows API.
#[cfg(windows)]
fn wide(path: &Path) -> Vec<u16> {
    use std::os::windows::ffi::OsStrExt;
    path.as_os_str()
        .encode_wide()
        .chain(std::iter::once(0))
        .collect()
}

/// Another launcher won the publication race: reload and compare, never overwrite.
fn resolve_race(folder: &Path, written: &Pin) -> io::Result<Pin> {
    match load(folder, &written.app_id)? {
        Some(existing) if existing == *written => Ok(existing),
        Some(_) => Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            format!("{} is already pinned to another key", written.app_id),
        )),
        None => Err(io::Error::new(
            io::ErrorKind::AlreadyExists,
            "the trust record disappeared while publishing",
        )),
    }
}

/// What the launcher knows about the package it asks trust for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrustQuestion {
    Signed { fingerprint: String },
    Unsigned { archive_digest: String },
}

/// The trust inputs the command line will fill (the wiring is the next task).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TrustInputs {
    /// `--trust-key`: accepted only when it equals the verified signer.
    pub trust_key: Option<String>,
    /// `--allow-unsigned`.
    pub allow_unsigned: bool,
}

/// What trust was established: a pinned fingerprint, or an unsigned allowance without a pin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TrustOutcome {
    Pinned(String),
    UnsignedAllowed,
}

/// Why trust was not established.
#[derive(Debug)]
pub enum TrustFailure {
    /// Exit 2: the inputs themselves are unusable.
    BadTrustKey(String),
    /// Exit 3: refused, changed, required, cancelled or nobody to ask.
    Refused(String),
    /// Exit 1: the store failed.
    Store(io::Error),
}

impl TrustFailure {
    pub fn exit(&self) -> u8 {
        match self {
            Self::BadTrustKey(_) => 2,
            Self::Refused(_) => 3,
            Self::Store(_) => 1,
        }
    }
}

impl fmt::Display for TrustFailure {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::BadTrustKey(message) => write!(f, "INVALID_ARGUMENT: {message}"),
            Self::Refused(message) => f.write_str(message),
            Self::Store(error) => write!(f, "{error}"),
        }
    }
}

/// The decision of trust, fail-closed: an already pinned key must match, a key given on the
/// command line must be the verified signer, an unsigned package needs the flag or a conspicuous
/// acceptance, and only a signed acceptance writes a pin.
pub fn decide(
    folder: &Path,
    app_id: &str,
    signer: Option<(&str, &str)>,
    archive_digest: &str,
    inputs: &TrustInputs,
    ask: &mut dyn FnMut(&TrustQuestion) -> Result<bool, String>,
) -> Result<TrustOutcome, TrustFailure> {
    match signer {
        Some((public_key, fingerprint)) => {
            if let Some(key) = &inputs.trust_key {
                if !hex64(key) {
                    return Err(TrustFailure::BadTrustKey(
                        "--trust-key must be the fingerprint of the signer of this package: \
                         it must be 64 lowercase hex"
                            .to_owned(),
                    ));
                }
                if key != fingerprint {
                    return Err(TrustFailure::BadTrustKey(
                        "--trust-key must be the fingerprint of the signer of this package: \
                         it does not match the signer of this package"
                            .to_owned(),
                    ));
                }
            }
            let existing = load(folder, app_id).map_err(TrustFailure::Store)?;
            match existing {
                Some(existing) if existing.public_key == public_key => {
                    Ok(TrustOutcome::Pinned(existing.fingerprint))
                }
                Some(existing) => Err(TrustFailure::Refused(format!(
                    "SIGNER_CHANGED: {app_id} is pinned to {old}; this package uses {new}",
                    old = existing.fingerprint,
                    new = fingerprint
                ))),
                None => {
                    let accept = match &inputs.trust_key {
                        Some(_) => true,
                        None => ask(&TrustQuestion::Signed {
                            fingerprint: fingerprint.to_owned(),
                        })
                        .map_err(TrustFailure::Refused)?,
                    };
                    if !accept {
                        return Err(TrustFailure::Refused(format!(
                            "TRUST_REFUSED: {app_id} was not accepted for this launch"
                        )));
                    }
                    pin(folder, app_id, public_key).map_err(TrustFailure::Store)?;
                    Ok(TrustOutcome::Pinned(fingerprint.to_owned()))
                }
            }
        }
        None => {
            let pinned = load(folder, app_id).map_err(TrustFailure::Store)?;
            if pinned.is_some() {
                return Err(TrustFailure::Refused(format!(
                    "SIGNATURE_REQUIRED: {app_id} was previously accepted with a signing key"
                )));
            }
            let accept = inputs.allow_unsigned
                || ask(&TrustQuestion::Unsigned {
                    archive_digest: archive_digest.to_owned(),
                })
                .map_err(TrustFailure::Refused)?;
            if !accept {
                return Err(TrustFailure::Refused(format!(
                    "TRUST_REFUSED: {app_id} was not accepted for this launch"
                )));
            }
            Ok(TrustOutcome::UnsignedAllowed)
        }
    }
}

/// End-to-end runs only (`ALEF_E2E=1`): the answer to the trust question, accept or refuse.
#[cfg(feature = "e2e")]
pub const TRUST_VARIABLE: &str = "ALEF_E2E_TRUST";

/// Who answers the trust question.
#[derive(Debug, Clone, PartialEq)]
pub enum TrustAsker {
    /// A fixed answer (end-to-end runs).
    #[cfg(any(test, feature = "e2e"))]
    Scripted(bool),
    /// The user, in the runtime-owned trust window.
    Window,
}

#[cfg(feature = "e2e")]
fn scripted_answer(read: &dyn Fn(&str) -> Option<String>) -> Result<Option<bool>, String> {
    if read("ALEF_E2E").is_some_and(|value| value == "1") {
        if let Some(text) = read(TRUST_VARIABLE) {
            return match text.as_str() {
                "accept" => Ok(Some(true)),
                "refuse" => Ok(Some(false)),
                other => Err(format!("{TRUST_VARIABLE}: not accept or refuse: {other:?}")),
            };
        }
    }
    Ok(None)
}

/// The scripted table applies only in an end-to-end run; otherwise the window asks.
pub fn asker_from_environment() -> Result<TrustAsker, String> {
    #[cfg(feature = "e2e")]
    if let Some(answer) = scripted_answer(&|name| std::env::var(name).ok())? {
        return Ok(TrustAsker::Scripted(answer));
    }
    Ok(TrustAsker::Window)
}

impl TrustAsker {
    pub fn ask(&self, app: &AppSummary, question: &TrustQuestion) -> Result<bool, String> {
        match self {
            #[cfg(any(test, feature = "e2e"))]
            Self::Scripted(answer) => Ok(*answer),
            Self::Window => window::ask_in_window(&window::request_for_trust(app, question))
                .and_then(|answered| match answered {
                    Some(answer) => Ok(answer),
                    None => Err("the trust window was closed without a decision".to_owned()),
                }),
        }
    }
}

/// [`decide`] with the asker of the environment: the scripted answer or the trust window.
pub fn decide_with_asker(
    folder: &Path,
    app_id: &str,
    app: &AppSummary,
    signer: Option<(&str, &str)>,
    archive_digest: &str,
    inputs: &TrustInputs,
    asker: &TrustAsker,
) -> Result<TrustOutcome, TrustFailure> {
    decide(
        folder,
        app_id,
        signer,
        archive_digest,
        inputs,
        &mut |question| asker.ask(app, question),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    const ID: &str = "example.app";

    fn key(character: char) -> String {
        std::iter::repeat(character).take(64).collect()
    }

    fn record_json(app_id: &str, public_key: &str, fingerprint: &str) -> Vec<u8> {
        format!(
            r#"{{"version":1,"app_id":"{app_id}","public_key":"{public_key}","fingerprint":"{fingerprint}"}}"#
        )
        .into_bytes()
    }

    #[test]
    fn a_missing_record_means_unpinned_and_a_valid_record_round_trips() {
        let folder = tempfile::tempdir().unwrap();
        assert_eq!(load(folder.path(), ID).unwrap(), None);
        let record = pin(folder.path(), ID, &key('a')).unwrap();
        assert_eq!(record.version, RECORD_VERSION);
        assert_eq!(record.app_id, ID);
        assert_eq!(record.public_key, key('a'));
        assert_eq!(record.fingerprint, fingerprint_of(&key('a')).unwrap());
        let path = pin_path(folder.path(), ID);
        assert!(path.exists());
        let stored: Pin = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(stored, record);
        assert_eq!(
            load(folder.path(), ID).unwrap(),
            Some(record),
            "the pinned record round-trips"
        );
    }

    #[test]
    fn a_pinned_key_is_refused_for_another_application_id() {
        let folder = tempfile::tempdir().unwrap();
        pin(folder.path(), ID, &key('a')).unwrap();
        assert_eq!(load(folder.path(), "other.id").unwrap(), None);
        assert_ne!(
            pin_path(folder.path(), "other.id"),
            pin_path(folder.path(), ID)
        );
    }

    #[test]
    fn a_malformed_or_foreign_record_is_an_error_never_first_use() {
        let folder = tempfile::tempdir().unwrap();
        let key = key('a');
        let fingerprint = fingerprint_of(&key).unwrap();
        let cases: Vec<Vec<u8>> = vec![
            b"not json".to_vec(),
            format!(
                r#"{{"version":1,"app_id":"{ID}","public_key":"{key}","fingerprint":"{fingerprint}","extra":1}}"#
            )
            .into_bytes(),
            format!(
                r#"{{"version":2,"app_id":"{ID}","public_key":"{key}","fingerprint":"{fingerprint}"}}"#
            )
            .into_bytes(),
            record_json("other.id", &key, &fingerprint),
            record_json(ID, "ABCD", &fingerprint),
            record_json(ID, &key, &"0".repeat(64)),
            Vec::new(),
        ];
        for bytes in cases {
            std::fs::write(pin_path(folder.path(), ID), &bytes).unwrap();
            let error = load(folder.path(), ID).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
        }
    }

    #[test]
    fn an_oversized_record_is_refused_before_it_is_parsed() {
        let folder = tempfile::tempdir().unwrap();
        std::fs::write(pin_path(folder.path(), ID), vec![b' '; 4097]).unwrap();
        assert_eq!(
            load(folder.path(), ID).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
    }

    #[test]
    fn links_and_directories_at_the_trust_boundary_are_refused() {
        // A directory where the record belongs is not a record.
        let folder = tempfile::tempdir().unwrap();
        std::fs::create_dir(pin_path(folder.path(), ID)).unwrap();
        assert_eq!(
            load(folder.path(), ID).unwrap_err().kind(),
            io::ErrorKind::InvalidData
        );
        // Windows reparse points are covered by the attribute check in `load`;
        // creating one needs privileges.
        #[cfg(unix)]
        {
            let folder = tempfile::tempdir().unwrap();
            let target = folder.path().join("target.json");
            std::fs::write(&target, b"{}").unwrap();
            std::os::unix::fs::symlink(&target, pin_path(folder.path(), ID)).unwrap();
            let error = load(folder.path(), ID).unwrap_err();
            assert_eq!(error.kind(), io::ErrorKind::InvalidData);
            assert!(error.to_string().contains("link"));
        }
    }

    #[cfg(unix)]
    #[test]
    fn an_unreadable_record_is_an_error() {
        let folder = tempfile::tempdir().unwrap();
        let target = folder.path().join("missing.json");
        std::os::unix::fs::symlink(&target, pin_path(folder.path(), ID)).unwrap();
        assert!(load(folder.path(), ID).is_err());
    }

    #[test]
    fn publication_is_no_clobber_and_a_competing_writer_is_reloaded_not_overwritten() {
        let folder = tempfile::tempdir().unwrap();
        let first = pin(folder.path(), ID, &key('a')).unwrap();
        assert_eq!(pin(folder.path(), ID, &key('a')).unwrap(), first);
        let error = pin(folder.path(), ID, &key('b')).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::AlreadyExists);
        assert!(error.to_string().contains("another key"));
        let stored: Pin =
            serde_json::from_slice(&std::fs::read(pin_path(folder.path(), ID)).unwrap()).unwrap();
        assert_eq!(stored, first);
        // Two threads race for the first pin with different keys: exactly one wins, the other
        // reloads the winner's record and is refused; the record is never replaced.
        let raced = tempfile::tempdir().unwrap();
        let [one, other] = race(raced.path(), 'a', 'b');
        let stored: Pin =
            serde_json::from_slice(&std::fs::read(pin_path(raced.path(), ID)).unwrap()).unwrap();
        let (winner, loser) = match (one, other) {
            (Ok(pinned), Err(error)) => {
                assert_eq!(stored.public_key, key('a'));
                (pinned, error)
            }
            (Err(error), Ok(pinned)) => {
                assert_eq!(stored.public_key, key('b'));
                (pinned, error)
            }
            (one, other) => panic!("exactly one writer must win: {one:?} and {other:?}"),
        };
        assert_eq!(winner, stored);
        assert_eq!(loser.kind(), io::ErrorKind::AlreadyExists);
        assert!(loser.to_string().contains("another key"));
        // The same key from two threads: both succeed with the same record.
        let same = tempfile::tempdir().unwrap();
        let [x, y] = race(same.path(), 'a', 'a');
        assert_eq!(x.unwrap(), y.unwrap());
    }

    fn race(folder: &std::path::Path, one: char, other: char) -> [io::Result<Pin>; 2] {
        let barrier = std::sync::Barrier::new(2);
        std::thread::scope(|scope| {
            let handles = [one, other].map(|mine| {
                let barrier = &barrier;
                scope.spawn(move || {
                    barrier.wait();
                    pin(folder, ID, &key(mine))
                })
            });
            handles.map(|handle| handle.join().unwrap())
        })
    }

    #[test]
    fn publication_leaves_no_temporary_files_behind() {
        let folder = tempfile::tempdir().unwrap();
        pin(folder.path(), ID, &key('a')).unwrap();
        assert_eq!(
            pin(folder.path(), ID, &key('b')).unwrap_err().kind(),
            io::ErrorKind::AlreadyExists
        );
        let raced = tempfile::tempdir().unwrap();
        let results = race(raced.path(), 'a', 'b');
        assert_eq!(results.iter().filter(|result| result.is_ok()).count(), 1);
        for folder in [folder.path(), raced.path()] {
            let expected = pin_path(folder, ID)
                .file_name()
                .unwrap()
                .to_str()
                .unwrap()
                .to_owned();
            let names: Vec<_> = std::fs::read_dir(folder)
                .unwrap()
                .map(|entry| entry.unwrap().file_name().to_str().unwrap().to_owned())
                .collect();
            assert_eq!(names, [expected]);
        }
    }

    #[cfg(windows)]
    #[test]
    fn the_windows_publication_refuses_an_existing_record_without_replacing_it() {
        let folder = tempfile::tempdir().unwrap();
        pin(folder.path(), ID, &key('a')).unwrap();
        let path = pin_path(folder.path(), ID);
        let backup = std::fs::read(&path).unwrap();
        assert!(pin(folder.path(), ID, &key('b')).is_err());
        assert_eq!(std::fs::read(&path).unwrap(), backup);
    }

    #[test]
    fn the_pin_file_name_is_the_digest_of_the_exact_id_and_ids_are_isolated() {
        let folder = Path::new("unused");
        for id in [ID, "other.id", "App.id", "app.id"] {
            let expected = folder.join(format!("{:x}.json", Sha256::digest(id.as_bytes())));
            assert_eq!(pin_path(folder, id), expected);
        }
        assert_ne!(pin_path(folder, "App.id"), pin_path(folder, "app.id"));
    }

    fn app() -> AppSummary {
        AppSummary {
            id: "org.example.app".to_owned(),
            name: "Example".to_owned(),
            version: "1.0.0".to_owned(),
        }
    }

    /// A recording asker: it remembers every question and always answers `answer`.
    fn recorder(
        questions: &std::cell::RefCell<Vec<TrustQuestion>>,
        answer: bool,
    ) -> impl FnMut(&TrustQuestion) -> Result<bool, String> + '_ {
        move |question| {
            questions.borrow_mut().push(question.clone());
            Ok(answer)
        }
    }

    fn fp(key: &str) -> String {
        fingerprint_of(key).expect("a hex64 key")
    }

    #[test]
    fn a_signed_unpinned_package_asks_and_acceptance_pins_the_key() {
        let folder = tempfile::tempdir().unwrap();
        let key = key('a');
        let questions = std::cell::RefCell::new(Vec::new());
        let outcome = decide(
            folder.path(),
            ID,
            Some((&key, &fp(&key))),
            "digest",
            &TrustInputs::default(),
            &mut recorder(&questions, true),
        )
        .unwrap();
        assert_eq!(outcome, TrustOutcome::Pinned(fp(&key)));
        let asked = questions.borrow();
        assert_eq!(
            *asked,
            vec![TrustQuestion::Signed {
                fingerprint: fp(&key)
            }]
        );
        let stored = load(folder.path(), ID).unwrap().expect("a pin");
        assert_eq!(stored.public_key, key);
    }

    #[test]
    fn a_trust_key_that_equals_the_signer_pins_without_asking() {
        let folder = tempfile::tempdir().unwrap();
        let key = key('a');
        let questions = std::cell::RefCell::new(Vec::new());
        let outcome = decide(
            folder.path(),
            ID,
            Some((&key, &fp(&key))),
            "digest",
            &TrustInputs {
                trust_key: Some(fp(&key)),
                allow_unsigned: false,
            },
            &mut recorder(&questions, true),
        )
        .unwrap();
        assert_eq!(outcome, TrustOutcome::Pinned(fp(&key)));
        assert!(questions.borrow().is_empty(), "nobody was asked");
        assert_eq!(
            load(folder.path(), ID).unwrap().expect("a pin").public_key,
            key
        );
    }

    #[test]
    fn a_trust_key_that_does_not_match_the_signer_is_refused_before_the_store() {
        let folder = tempfile::tempdir().unwrap();
        let key = key('a');
        let questions = std::cell::RefCell::new(Vec::new());
        // (a) Not even hex.
        let failure = decide(
            folder.path(),
            ID,
            Some((&key, &fp(&key))),
            "digest",
            &TrustInputs {
                trust_key: Some("nothex".to_owned()),
                allow_unsigned: false,
            },
            &mut recorder(&questions, true),
        )
        .unwrap_err();
        assert!(matches!(failure, TrustFailure::BadTrustKey(_)));
        assert_eq!(failure.exit(), 2);
        // (b) Valid hex, but another key.
        let other = "b".repeat(64);
        let failure = decide(
            folder.path(),
            ID,
            Some((&key, &fp(&key))),
            "digest",
            &TrustInputs {
                trust_key: Some(other),
                allow_unsigned: false,
            },
            &mut recorder(&questions, true),
        )
        .unwrap_err();
        assert!(matches!(failure, TrustFailure::BadTrustKey(_)));
        assert_eq!(failure.to_string(), format!("INVALID_ARGUMENT: --trust-key must be the fingerprint of the signer of this package: it does not match the signer of this package"));
        assert!(questions.borrow().is_empty());
        assert_eq!(load(folder.path(), ID).unwrap(), None, "no pin");
    }

    #[test]
    fn a_pinned_matching_signer_proceeds_without_asking() {
        let folder = tempfile::tempdir().unwrap();
        let key = key('a');
        pin(folder.path(), ID, &key).unwrap();
        let questions = std::cell::RefCell::new(Vec::new());
        let outcome = decide(
            folder.path(),
            ID,
            Some((&key, &fp(&key))),
            "digest",
            &TrustInputs::default(),
            &mut recorder(&questions, false),
        )
        .unwrap();
        assert_eq!(outcome, TrustOutcome::Pinned(fp(&key)));
        assert!(questions.borrow().is_empty(), "nobody was asked");
    }

    #[test]
    fn a_pin_to_another_key_is_signer_changed_and_the_pin_is_kept() {
        let folder = tempfile::tempdir().unwrap();
        let key = key('a');
        let other = "b".repeat(64);
        pin(folder.path(), ID, &key).unwrap();
        let questions = std::cell::RefCell::new(Vec::new());
        let failure = decide(
            folder.path(),
            ID,
            Some((&other, &fp(&other))),
            "digest",
            &TrustInputs::default(),
            &mut recorder(&questions, true),
        )
        .unwrap_err();
        let message = failure.to_string();
        assert!(message.starts_with("SIGNER_CHANGED: "), "{message}");
        assert!(message.contains(ID));
        assert!(message.contains(&fp(&key)), "{message}");
        assert!(message.contains(&fp(&other)), "{message}");
        assert_eq!(failure.exit(), 3);
        assert_eq!(
            load(folder.path(), ID)
                .unwrap()
                .expect("the pin is kept")
                .public_key,
            key
        );
    }

    #[test]
    fn an_unsigned_package_after_a_signed_pin_is_signature_required_even_with_allow_unsigned() {
        let folder = tempfile::tempdir().unwrap();
        pin(folder.path(), ID, &key('a')).unwrap();
        let questions = std::cell::RefCell::new(Vec::new());
        let failure = decide(
            folder.path(),
            ID,
            None,
            "digest",
            &TrustInputs {
                trust_key: None,
                allow_unsigned: true,
            },
            &mut recorder(&questions, true),
        )
        .unwrap_err();
        assert!(failure.to_string().starts_with("SIGNATURE_REQUIRED: "),);
        assert_eq!(failure.exit(), 3);
    }

    #[test]
    fn an_unsigned_package_needs_a_conspicuous_acceptance_or_the_flag_and_never_pins() {
        let folder = tempfile::tempdir().unwrap();
        // (a) The flag allows without asking, and nothing is pinned.
        let questions = std::cell::RefCell::new(Vec::new());
        let outcome = decide(
            folder.path(),
            ID,
            None,
            "digest",
            &TrustInputs {
                trust_key: None,
                allow_unsigned: true,
            },
            &mut recorder(&questions, true),
        )
        .unwrap();
        assert_eq!(outcome, TrustOutcome::UnsignedAllowed);
        assert!(questions.borrow().is_empty());
        assert_eq!(load(folder.path(), ID).unwrap(), None);

        // (b) An acceptance allows, and nothing is pinned.
        let questions = std::cell::RefCell::new(Vec::new());
        let outcome = decide(
            folder.path(),
            ID,
            None,
            "digest",
            &TrustInputs::default(),
            &mut recorder(&questions, true),
        )
        .unwrap();
        assert_eq!(outcome, TrustOutcome::UnsignedAllowed);
        assert_eq!(
            *questions.borrow(),
            vec![TrustQuestion::Unsigned {
                archive_digest: "digest".to_owned()
            }]
        );
        assert_eq!(load(folder.path(), ID).unwrap(), None);

        // (c) A refusal ends the launch with code 3.
        let questions = std::cell::RefCell::new(Vec::new());
        let failure = decide(
            folder.path(),
            ID,
            None,
            "digest",
            &TrustInputs::default(),
            &mut recorder(&questions, false),
        )
        .unwrap_err();
        assert!(failure.to_string().starts_with("TRUST_REFUSED: "));
        assert_eq!(failure.exit(), 3);
        assert_eq!(load(folder.path(), ID).unwrap(), None);
    }

    #[test]
    fn an_acceptance_that_fails_to_publish_is_a_store_failure() {
        // A folder that is a file: `pin` cannot create its directory inside it.
        let folder = tempfile::tempdir().unwrap();
        let blocked = folder.path().join("blocked");
        std::fs::write(&blocked, b"not a folder").unwrap();
        let key = key('a');
        let questions = std::cell::RefCell::new(Vec::new());
        let failure = decide(
            &blocked,
            ID,
            Some((&key, &fp(&key))),
            "digest",
            &TrustInputs::default(),
            &mut recorder(&questions, true),
        )
        .unwrap_err();
        assert!(matches!(failure, TrustFailure::Store(_)), "{failure}");
        assert_eq!(failure.exit(), 1);
    }

    #[test]
    fn decide_with_asker_passes_the_summary_of_the_application_to_the_window() {
        let folder = tempfile::tempdir().unwrap();
        let key = key('a');
        // The scripted asker answers on its own; the window path is the next task's wiring.
        let outcome = decide_with_asker(
            folder.path(),
            ID,
            &app(),
            Some((&key, &fp(&key))),
            "digest",
            &TrustInputs::default(),
            &TrustAsker::Scripted(true),
        )
        .unwrap();
        assert_eq!(outcome, TrustOutcome::Pinned(fp(&key)));
    }

    #[cfg(feature = "e2e")]
    #[test]
    fn the_scripted_trust_answer_is_read_only_in_an_end_to_end_run() {
        fn pairs(
            table: &'static [(&'static str, &'static str)],
        ) -> impl Fn(&str) -> Option<String> {
            move |name| {
                table
                    .iter()
                    .find(|(key, _)| *key == name)
                    .map(|(_, value)| (*value).to_owned())
            }
        }
        let silent = pairs(&[(TRUST_VARIABLE, "accept")]);
        assert_eq!(scripted_answer(&silent).unwrap(), None);
        let off = pairs(&[("ALEF_E2E", "0"), (TRUST_VARIABLE, "accept")]);
        assert_eq!(scripted_answer(&off).unwrap(), None);
        let accept = pairs(&[("ALEF_E2E", "1"), (TRUST_VARIABLE, "accept")]);
        assert_eq!(scripted_answer(&accept).unwrap(), Some(true));
        let refuse = pairs(&[("ALEF_E2E", "1"), (TRUST_VARIABLE, "refuse")]);
        assert_eq!(scripted_answer(&refuse).unwrap(), Some(false));
        let nothing = pairs(&[("ALEF_E2E", "1")]);
        assert_eq!(scripted_answer(&nothing).unwrap(), None);
        let other = pairs(&[("ALEF_E2E", "1"), (TRUST_VARIABLE, "maybe")]);
        assert!(scripted_answer(&other).is_err());
    }
}
