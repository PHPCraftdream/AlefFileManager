// SPDX-License-Identifier: MIT OR Apache-2.0
//! What the user decided about the rights an application asks for. The manifest says what may be
//! asked; the decision says what is given: the real thing, a stand-in that the application cannot
//! tell from the real thing, or nothing.
use std::{
    collections::BTreeMap,
    fmt, fs, io,
    path::{Path, PathBuf},
    str::FromStr,
};

use serde::{Deserialize, Serialize};

/// What the user gives for a right.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, ts_rs::TS)]
#[serde(rename_all = "lowercase")]
#[ts(export, export_to = "core.ts")]
pub enum Decision {
    /// The real access.
    Allow,
    /// A stand-in: the application gets a plausible environment and no error code that gives it
    /// away (the variable is not set, the clipboard is the application's own, nothing is opened).
    Substitute,
    /// `PERMISSION_DENIED`.
    Deny,
}

impl Decision {
    /// The more restrictive of two decisions: deny, then substitute, then allow.
    pub fn stricter(self, other: Self) -> Self {
        let rank = |decision| match decision {
            Self::Allow => 0,
            Self::Substitute => 1,
            Self::Deny => 2,
        };
        if rank(other) > rank(self) {
            other
        } else {
            self
        }
    }
}

/// One thing the manifest asks for, as the consent window shows it and the store remembers it:
/// the name of the permission and, where it has scopes, one scope as the manifest wrote it.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize, ts_rs::TS)]
#[ts(export, export_to = "core.ts")]
pub struct Right {
    /// `fs.read`, `net.http`, `app.env`, `clipboard.read`, ...
    pub permission: String,
    /// The scope entry (`$DOCUMENTS/**`, `https://example.com/*`, an environment variable name);
    /// `None` for a right without scopes (`clipboard.read`, `secrets`).
    #[ts(optional)]
    pub scope: Option<String>,
}

impl Right {
    pub fn plain(permission: &str) -> Self {
        Self {
            permission: permission.to_owned(),
            scope: None,
        }
    }

    pub fn scoped(permission: &str, scope: &str) -> Self {
        Self {
            permission: permission.to_owned(),
            scope: Some(scope.to_owned()),
        }
    }

    /// Why the user should think twice before allowing this right, in words for the consent window;
    /// `None` for a right that reaches no further than it says. Allowing a risky right takes a
    /// confirmation of its own.
    pub fn risk(&self) -> Option<&'static str> {
        let scope = self.scope.as_deref()?;
        match self.permission.as_str() {
            "cli.exec" if scope == "*" => Some("runs any program on this computer"),
            "fs.read" if reaches_everything(scope) => {
                Some("reads anything in the home folder or on the whole disk")
            }
            "fs.write" if reaches_everything(scope) => {
                Some("changes anything in the home folder or on the whole disk")
            }
            "net.http" if any_address(scope) => Some("talks to any address on the internet"),
            _ => None,
        }
    }
}

/// A scope that is the whole disk or the whole home folder.
fn reaches_everything(scope: &str) -> bool {
    let scope = scope.replace('\\', "/");
    let drive_root = {
        let bytes = scope.as_bytes();
        bytes.len() >= 4
            && bytes[0].is_ascii_alphabetic()
            && bytes[1] == b':'
            && bytes[2] == b'/'
            && matches!(&scope[3..], "*" | "**")
    };
    drive_root
        || matches!(
            scope.as_str(),
            "**" | "/**" | "/*" | "$HOME" | "$HOME/*" | "$HOME/**"
        )
}

/// A URL scope whose host is a wildcard.
fn any_address(scope: &str) -> bool {
    let Some((_, rest)) = scope.split_once("://") else {
        return false;
    };
    rest.split('/').next() == Some("*")
}

/// `permission` or `permission:scope` (a scope may contain colons: the first one separates).
impl fmt::Display for Right {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.scope {
            Some(scope) => write!(f, "{}:{scope}", self.permission),
            None => f.write_str(&self.permission),
        }
    }
}

impl FromStr for Right {
    type Err = String;

    fn from_str(text: &str) -> Result<Self, String> {
        let (permission, scope) = match text.split_once(':') {
            Some((permission, scope)) => (permission, Some(scope)),
            None => (text, None),
        };
        let named = !permission.is_empty()
            && !permission.starts_with('.')
            && !permission.ends_with('.')
            && permission
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || c == '.');
        if !named {
            return Err(format!("not a right: {text:?}"));
        }
        if scope == Some("") {
            return Err(format!("not a right (empty scope): {text:?}"));
        }
        Ok(Self {
            permission: permission.to_owned(),
            scope: scope.map(str::to_owned),
        })
    }
}

/// The decisions of the user for one application. A right that was never decided gets the
/// fallback: everything for an embedding host that asks nobody, nothing for an application whose
/// user has not been asked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Consent {
    decisions: BTreeMap<Right, Decision>,
    fallback: Decision,
}

impl Consent {
    /// No question was asked and none will be: every right of the manifest is given.
    pub fn allow_all() -> Self {
        Self {
            decisions: BTreeMap::new(),
            fallback: Decision::Allow,
        }
    }

    /// A user who has not been asked: whatever is not decided is denied.
    pub fn undecided() -> Self {
        Self {
            decisions: BTreeMap::new(),
            fallback: Decision::Deny,
        }
    }

    pub fn set(&mut self, right: Right, decision: Decision) {
        self.decisions.insert(right, decision);
    }

    pub fn decision(&self, right: &Right) -> Decision {
        self.decisions.get(right).copied().unwrap_or(self.fallback)
    }

    /// Whether the user has decided on `right`.
    pub fn decided(&self, right: &Right) -> bool {
        self.decisions.contains_key(right)
    }

    /// The rights of `wanted` that the user has not decided on yet, in order.
    pub fn missing<'a>(&self, wanted: &'a [Right]) -> Vec<&'a Right> {
        wanted.iter().filter(|right| !self.decided(right)).collect()
    }

    /// Keeps only the decisions for rights in `wanted`: a right the manifest no longer asks for
    /// leaves no decision behind that a later manifest could pick up unasked.
    pub fn retain(&mut self, wanted: &[Right]) {
        self.decisions.retain(|right, _| wanted.contains(right));
    }

    pub fn decisions(&self) -> impl Iterator<Item = (&Right, Decision)> {
        self.decisions
            .iter()
            .map(|(right, decision)| (right, *decision))
    }
}

/// Who the decisions belong to: the application and the key its package was signed with. A package
/// of the same `id` signed by another key inherits nothing.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Identity {
    pub app_id: String,
    /// Fingerprint of the signing key; `None` for an application run from a folder.
    pub signer: Option<String>,
}

#[derive(Serialize, Deserialize)]
struct Entry {
    permission: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    scope: Option<String>,
    decision: Decision,
}

#[derive(Serialize, Deserialize)]
struct Saved {
    version: u32,
    app_id: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    signer: Option<String>,
    decisions: Vec<Entry>,
}

const VERSION: u32 = 1;

/// Where the decisions of the users are kept. They are the user's, not the application's: a store
/// lives in the data folder of the runtime, outside every scope of every application.
pub trait ConsentStore: Send + Sync {
    /// The decisions made for `identity`; `None` when the user was never asked.
    fn load(&self, identity: &Identity) -> io::Result<Option<Consent>>;
    fn save(&self, identity: &Identity, consent: &Consent) -> io::Result<()>;
    /// Forgets the decisions: the next start asks again.
    fn forget(&self, identity: &Identity) -> io::Result<()>;
    /// Every identity that has decisions.
    fn identities(&self) -> io::Result<Vec<Identity>>;
}

/// A store of one JSON file per identity in a folder.
#[derive(Debug, Clone)]
pub struct FileConsentStore {
    folder: PathBuf,
}

/// Letters, digits, `.` and `-` stay; anything else becomes `_`. The signer is a fingerprint of hex
/// digits, the id a reverse domain name: no two identities share a file name unless both parts
/// are equal after this (the file repeats them, and a mismatch is not believed).
fn file_part(text: &str) -> String {
    let plain: String = text
        .chars()
        .map(|c| {
            if c.is_ascii_alphanumeric() || matches!(c, '.' | '-') {
                c
            } else {
                '_'
            }
        })
        .collect();
    if plain.len() <= LONGEST_PART {
        return plain;
    }
    // A long part (a folder path) keeps its head and a hash of the whole: two long parts that
    // collide share a file, and the identity inside the file tells them apart.
    let hash = text.bytes().fold(0xcbf2_9ce4_8422_2325_u64, |hash, byte| {
        (hash ^ u64::from(byte)).wrapping_mul(0x0100_0000_01b3)
    });
    format!("{}-{hash:016x}", &plain[..HEAD_OF_LONG_PART])
}

const LONGEST_PART: usize = 48;
const HEAD_OF_LONG_PART: usize = 24;

impl FileConsentStore {
    pub fn new(folder: impl Into<PathBuf>) -> Self {
        Self {
            folder: folder.into(),
        }
    }

    fn path(&self, identity: &Identity) -> PathBuf {
        let signer = identity
            .signer
            .as_deref()
            .map_or_else(|| "folder".to_owned(), file_part);
        self.folder
            .join(format!("{}@{signer}.json", file_part(&identity.app_id)))
    }

    fn read(path: &Path) -> io::Result<Option<(Identity, Consent)>> {
        let bytes = match fs::read(path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
            Err(error) => return Err(error),
        };
        // A file that is damaged or of another version is as good as no decisions: the user is asked again.
        let Ok(saved) = serde_json::from_slice::<Saved>(&bytes) else {
            return Ok(None);
        };
        if saved.version != VERSION {
            return Ok(None);
        }
        let mut consent = Consent::undecided();
        for entry in saved.decisions {
            consent.set(
                Right {
                    permission: entry.permission,
                    scope: entry.scope,
                },
                entry.decision,
            );
        }
        let identity = Identity {
            app_id: saved.app_id,
            signer: saved.signer,
        };
        Ok(Some((identity, consent)))
    }
}

impl ConsentStore for FileConsentStore {
    fn load(&self, identity: &Identity) -> io::Result<Option<Consent>> {
        Ok(Self::read(&self.path(identity))?
            .filter(|(found, _)| found == identity)
            .map(|(_, consent)| consent))
    }

    fn save(&self, identity: &Identity, consent: &Consent) -> io::Result<()> {
        let saved = Saved {
            version: VERSION,
            app_id: identity.app_id.clone(),
            signer: identity.signer.clone(),
            decisions: consent
                .decisions()
                .map(|(right, decision)| Entry {
                    permission: right.permission.clone(),
                    scope: right.scope.clone(),
                    decision,
                })
                .collect(),
        };
        let text = serde_json::to_vec_pretty(&saved).map_err(io::Error::other)?;
        fs::create_dir_all(&self.folder)?;
        let path = self.path(identity);
        let partial = path.with_extension("json.part");
        fs::write(&partial, text)?;
        fs::rename(&partial, &path)
    }

    fn forget(&self, identity: &Identity) -> io::Result<()> {
        match fs::remove_file(self.path(identity)) {
            Err(error) if error.kind() != io::ErrorKind::NotFound => Err(error),
            _ => Ok(()),
        }
    }

    fn identities(&self) -> io::Result<Vec<Identity>> {
        let entries = match fs::read_dir(&self.folder) {
            Ok(entries) => entries,
            Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(Vec::new()),
            Err(error) => return Err(error),
        };
        let mut found = Vec::new();
        for entry in entries {
            let path = entry?.path();
            if path
                .extension()
                .is_some_and(|extension| extension == "json")
            {
                if let Some((identity, _)) = Self::read(&path)? {
                    found.push(identity);
                }
            }
        }
        found.sort_by(|a, b| (&a.app_id, &a.signer).cmp(&(&b.app_id, &b.signer)));
        Ok(found)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn right(permission: &str, scope: &str) -> Right {
        Right::scoped(permission, scope)
    }

    fn identity(id: &str, signer: Option<&str>) -> Identity {
        Identity {
            app_id: id.to_owned(),
            signer: signer.map(str::to_owned),
        }
    }

    #[test]
    fn a_right_that_was_never_decided_gets_the_fallback() {
        let mut open = Consent::allow_all();
        let mut shut = Consent::undecided();
        let asked = right("fs.read", "$DOCUMENTS/**");
        assert_eq!(open.decision(&asked), Decision::Allow);
        assert_eq!(shut.decision(&asked), Decision::Deny);
        assert!(!shut.decided(&asked));
        open.set(asked.clone(), Decision::Deny);
        shut.set(asked.clone(), Decision::Substitute);
        assert_eq!(open.decision(&asked), Decision::Deny);
        assert_eq!(shut.decision(&asked), Decision::Substitute);
        assert!(shut.decided(&asked));
        assert_eq!(
            shut.decision(&right("fs.read", "$HOME/**")),
            Decision::Deny,
            "another scope is another right"
        );
    }

    #[test]
    fn what_is_missing_is_what_the_manifest_asks_and_the_user_has_not_decided() {
        let wanted = [
            Right::plain("clipboard.read"),
            right("app.env", "HOME"),
            right("app.env", "PATH"),
        ];
        let mut consent = Consent::undecided();
        consent.set(wanted[1].clone(), Decision::Allow);
        assert_eq!(consent.missing(&wanted), [&wanted[0], &wanted[2]]);
        consent.retain(&wanted[..1]);
        assert!(
            !consent.decided(&wanted[1]),
            "a right the manifest dropped leaves no decision behind"
        );
    }

    #[test]
    fn decisions_survive_a_save_and_a_load_and_belong_to_one_identity() {
        let folder = tempfile::tempdir().unwrap();
        let store = FileConsentStore::new(folder.path().join("consent"));
        let mine = identity("org.example.app", None);
        assert_eq!(store.load(&mine).unwrap(), None, "never asked");
        let mut consent = Consent::undecided();
        consent.set(right("fs.read", "$DOCUMENTS/**"), Decision::Substitute);
        consent.set(Right::plain("clipboard.read"), Decision::Deny);
        store.save(&mine, &consent).unwrap();
        assert_eq!(store.load(&mine).unwrap(), Some(consent.clone()));
        assert_eq!(
            store
                .load(&identity("org.example.app", Some("abcd")))
                .unwrap(),
            None,
            "the same id signed by another key inherits nothing"
        );
        assert_eq!(
            store.load(&identity("org.example.other", None)).unwrap(),
            None
        );
        assert_eq!(store.identities().unwrap(), std::slice::from_ref(&mine));
        store.forget(&mine).unwrap();
        assert_eq!(store.load(&mine).unwrap(), None);
        store.forget(&mine).unwrap();
        assert!(store.identities().unwrap().is_empty());
    }

    #[test]
    fn a_damaged_file_a_foreign_version_or_a_renamed_file_is_not_believed() {
        let folder = tempfile::tempdir().unwrap();
        let store = FileConsentStore::new(folder.path());
        let mine = identity("org.example.app", None);
        let mut consent = Consent::undecided();
        consent.set(Right::plain("secrets"), Decision::Allow);
        store.save(&mine, &consent).unwrap();
        let path = folder.path().join("org.example.app@folder.json");
        assert!(path.exists());
        fs::write(&path, "not json").unwrap();
        assert_eq!(store.load(&mine).unwrap(), None);
        fs::write(
            &path,
            r#"{"version": 9, "app_id": "org.example.app", "decisions": []}"#,
        )
        .unwrap();
        assert_eq!(store.load(&mine).unwrap(), None);
        // A file that was put under another identity's name does not speak for that identity.
        fs::write(
            &path,
            r#"{"version": 1, "app_id": "org.example.evil", "decisions": [{"permission": "secrets", "decision": "allow"}]}"#,
        )
        .unwrap();
        assert_eq!(store.load(&mine).unwrap(), None);
    }

    #[test]
    fn a_right_that_reaches_everything_is_risky_and_one_that_says_what_it_reaches_is_not() {
        for risky in [
            ("cli.exec", "*"),
            ("fs.read", "$HOME/**"),
            ("fs.read", "$HOME"),
            ("fs.write", "**"),
            ("fs.write", "/**"),
            ("fs.read", "C:/**"),
            ("fs.read", r"D:\**"),
            ("net.http", "https://*/*"),
            ("net.http", "http://*"),
        ] {
            assert!(
                right(risky.0, risky.1).risk().is_some(),
                "{}:{}",
                risky.0,
                risky.1
            );
        }
        for plain in [
            ("cli.exec", "git"),
            ("fs.read", "$DOCUMENTS/**"),
            ("fs.read", "$HOME/notes/**"),
            ("fs.write", "$APPDATA/**"),
            ("net.http", "https://example.com/*"),
            ("net.http", "https://*.example.com/*"),
            ("app.env", "*"),
            ("shell.openExternal", "https://*/*"),
        ] {
            assert!(
                right(plain.0, plain.1).risk().is_none(),
                "{}:{}",
                plain.0,
                plain.1
            );
        }
        assert!(Right::plain("clipboard.read").risk().is_none());
        assert!(
            right("fs.read", "$HOME/**").risk() != right("fs.write", "$HOME/**").risk(),
            "reading and writing are told apart"
        );
    }

    #[test]
    fn a_right_is_written_as_text_and_read_back() {
        let plain: Right = "clipboard.read".parse().unwrap();
        assert_eq!(plain, Right::plain("clipboard.read"));
        assert_eq!(plain.to_string(), "clipboard.read");
        let url: Right = "net.http:https://example.com/*".parse().unwrap();
        assert_eq!(url, Right::scoped("net.http", "https://example.com/*"));
        assert_eq!(url.to_string(), "net.http:https://example.com/*");
        let path: Right = "fs.read:$DOCUMENTS/**".parse().unwrap();
        assert_eq!(path.scope.as_deref(), Some("$DOCUMENTS/**"));
        for bad in ["", ":x", "fs.read:", ".fs", "fs.", "fs read", "fs/read:x"] {
            assert!(bad.parse::<Right>().is_err(), "{bad:?}");
        }
    }

    #[test]
    fn a_long_identity_gets_a_short_file_name_and_still_its_own_decisions() {
        let folder = tempfile::tempdir().unwrap();
        let store = FileConsentStore::new(folder.path());
        let base = "dir:/home/someone/a/rather/long/path/to/an/application/folder";
        let one = identity("org.example.app", Some(&format!("{base}-one")));
        let two = identity("org.example.app", Some(&format!("{base}-two")));
        let mut consent = Consent::undecided();
        consent.set(Right::plain("secrets"), Decision::Allow);
        store.save(&one, &consent).unwrap();
        let names: Vec<String> = fs::read_dir(folder.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(names.len(), 1);
        assert!(names[0].len() < 100, "{}", names[0]);
        assert_eq!(store.load(&one).unwrap(), Some(consent));
        assert_eq!(
            store.load(&two).unwrap(),
            None,
            "another folder, nothing inherited"
        );
    }

    #[test]
    fn identities_that_differ_only_in_odd_characters_get_files_of_their_own_names() {
        let folder = tempfile::tempdir().unwrap();
        let store = FileConsentStore::new(folder.path());
        let odd = identity("../evil/app", Some("a b"));
        store.save(&odd, &Consent::undecided()).unwrap();
        let names: Vec<String> = fs::read_dir(folder.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
            .collect();
        assert_eq!(
            names,
            [".._evil_app@a_b.json"],
            "nothing is written outside the folder"
        );
        assert_eq!(store.load(&odd).unwrap().map(|_| ()), Some(()));
    }
}
