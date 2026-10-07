// SPDX-License-Identifier: MIT OR Apache-2.0
//! The launcher's side of consent: where the decisions of the user are kept, whom they belong to,
//! and how the rights an application asks for are decided before it starts.
use std::{
    collections::BTreeMap,
    fmt,
    path::{Path, PathBuf},
    sync::Arc,
    time::Duration,
};

use alef_core::{
    error::{AlefError, ErrorCode},
    security::{
        consent::{Consent, ConsentStore, Decision, FileConsentStore, Identity, Right},
        permissions::PermissionSet,
    },
};
use serde_json::Value;

pub mod ask;
#[cfg(test)]
pub(crate) mod fixtures;

use ask::{ask_in_window, request_for, AppSummary};

/// End-to-end runs only (`ALEF_E2E=1`): the clicks of the user in the consent window, as JSON.
pub const UI_VARIABLE: &str = "ALEF_E2E_CONSENT_UI";

/// Moves the data folder of the runtime (decisions of the users, among others).
pub const HOME_VARIABLE: &str = "ALEF_HOME";

/// End-to-end runs only (`ALEF_E2E=1`): the answers to the questions, `right=decision;...`.
pub const SCRIPT_VARIABLE: &str = "ALEF_E2E_CONSENT";

/// The data folder of the runtime: nothing in it belongs to an application.
pub fn runtime_home() -> PathBuf {
    if let Some(home) = std::env::var_os(HOME_VARIABLE)
        .map(PathBuf::from)
        .filter(|home| home.is_absolute())
    {
        return home;
    }
    dirs::data_local_dir()
        .or_else(|| dirs::home_dir().map(|home| home.join(".local").join("share")))
        .unwrap_or_else(std::env::temp_dir)
        .join("alef")
}

/// Where the stand-ins of the application keep their content: a folder of the runtime under the
/// key of the identity, so that another folder or another key does not find it.
pub fn shadow_folder(identity: &Identity) -> PathBuf {
    runtime_home().join("shadow").join(identity.key())
}

/// The decisions of the users, one file per application.
pub fn store() -> FileConsentStore {
    FileConsentStore::new(runtime_home().join("consent"))
}

/// Whom the decisions belong to. An application run from a folder is known by its id and where it
/// lies: another folder with the same id inherits nothing. A signed package (M7) is known by the
/// key of its signature instead.
pub fn identity_of(app_dir: &Path, id: &str) -> Result<Identity, AlefError> {
    let app = std::fs::canonicalize(app_dir).map_err(|error| {
        AlefError::new(
            ErrorCode::NotFound,
            format!("cannot open the application directory: {error}"),
        )
    })?;
    Ok(Identity {
        app_id: id.to_owned(),
        signer: Some(format!("dir:{}", app.display())),
    })
}

/// The answers to the questions: `right=decision` separated by `;`, and `*=decision` for every
/// right not named. A right that is neither named nor covered by `*` stays unanswered.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Script {
    named: BTreeMap<Right, Decision>,
    rest: Option<Decision>,
}

pub fn decision_named(word: &str) -> Result<Decision, String> {
    match word {
        "allow" => Ok(Decision::Allow),
        "substitute" => Ok(Decision::Substitute),
        "deny" => Ok(Decision::Deny),
        other => Err(format!(
            "not a decision: {other:?} (allow, substitute or deny)"
        )),
    }
}

/// The word the user types and reads for a decision.
pub fn word_of(decision: Decision) -> &'static str {
    match decision {
        Decision::Allow => "allow",
        Decision::Substitute => "substitute",
        Decision::Deny => "deny",
    }
}

impl Script {
    pub fn parse(text: &str) -> Result<Self, String> {
        let mut script = Self::default();
        for entry in text.split(';').map(str::trim).filter(|e| !e.is_empty()) {
            let (right, word) = entry
                .rsplit_once('=')
                .ok_or_else(|| format!("not right=decision: {entry:?}"))?;
            let decision = decision_named(word.trim())?;
            if right.trim() == "*" {
                script.rest = Some(decision);
            } else {
                script.named.insert(right.trim().parse()?, decision);
            }
        }
        Ok(script)
    }

    fn decide(&self, right: &Right) -> Option<Decision> {
        self.named.get(right).copied().or(self.rest)
    }
}

/// Whoever answers the questions of the user.
#[derive(Debug, Clone, PartialEq)]
pub enum Asker {
    /// `--grant`: every right not decided yet gets this decision.
    Grant(Decision),
    /// A fixed table of answers (end-to-end runs).
    Script(Script),
    /// The user, in the consent window; `automation` is the clicks that an end-to-end run plays.
    Window { automation: Option<Value> },
    /// Nobody to ask: the application does not start with rights nobody decided on.
    Nobody,
}

/// Whether a window can be shown at all: a desktop session on Windows and macOS, a display on the
/// other systems.
fn can_show_a_window() -> bool {
    cfg!(any(target_os = "windows", target_os = "macos"))
        || std::env::var_os("DISPLAY").is_some_and(|value| !value.is_empty())
        || std::env::var_os("WAYLAND_DISPLAY").is_some_and(|value| !value.is_empty())
}

impl Asker {
    /// `--grant` wins; the table and the clicks of the end-to-end runs apply only with `ALEF_E2E=1`;
    /// `--no-prompt` and a machine that cannot show a window leave nobody to ask.
    pub fn from_environment(grant: Option<Decision>, no_prompt: bool) -> Result<Self, String> {
        if let Some(decision) = grant {
            return Ok(Self::Grant(decision));
        }
        let end_to_end = std::env::var("ALEF_E2E").is_ok_and(|value| value == "1");
        if end_to_end {
            if let Ok(text) = std::env::var(SCRIPT_VARIABLE) {
                return Script::parse(&text)
                    .map(Self::Script)
                    .map_err(|error| format!("{SCRIPT_VARIABLE}: {error}"));
            }
        }
        if no_prompt || !can_show_a_window() {
            return Ok(Self::Nobody);
        }
        let automation = match std::env::var(UI_VARIABLE) {
            Ok(text) if end_to_end => Some(
                serde_json::from_str(&text).map_err(|error| format!("{UI_VARIABLE}: {error}"))?,
            ),
            _ => None,
        };
        Ok(Self::Window { automation })
    }
}

/// The decisions the application starts with.
#[derive(Debug)]
pub struct Settled {
    pub consent: Consent,
    /// Whether the store is to be written: something was decided, or a right was dropped.
    pub changed: bool,
}

/// Why rights stay undecided.
#[derive(Debug, PartialEq, Eq)]
pub enum Why {
    /// Nobody could be asked.
    Nobody,
    /// The user closed the window without deciding.
    Cancelled,
    /// The window could not be shown or its answer not read.
    Failed(String),
}

/// Rights nobody decided on: the application does not start.
#[derive(Debug, PartialEq, Eq)]
pub struct Unanswered {
    pub rights: Vec<Right>,
    pub why: Why,
}

impl fmt::Display for Unanswered {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match &self.why {
            Why::Cancelled => f.write_str("the permission window was closed without a decision")?,
            Why::Failed(reason) => write!(f, "the permission window failed: {reason}")?,
            Why::Nobody => {
                write!(f, "the application asks for rights nobody has decided on: ")?;
                for (index, right) in self.rights.iter().enumerate() {
                    if index > 0 {
                        f.write_str(", ")?;
                    }
                    write!(f, "{right}")?;
                }
                write!(
                    f,
                    ". Decide them with `alef permissions set` or start with `--grant allow|substitute|deny`."
                )?;
            }
        }
        Ok(())
    }
}

/// Puts the stored decisions and the rights the manifest asks for together: whatever is new is
/// asked, whatever the manifest dropped is forgotten.
pub fn settle(
    app: &AppSummary,
    wanted: &[Right],
    stored: Option<Consent>,
    asker: &Asker,
) -> Result<Settled, Unanswered> {
    let mut consent = stored.unwrap_or_else(Consent::undecided);
    let new: Vec<Right> = consent.missing(wanted).into_iter().cloned().collect();
    let dropped = consent
        .decisions()
        .any(|(right, _)| !wanted.contains(right));
    if !new.is_empty() {
        let unanswered = |why| Unanswered {
            rights: new.clone(),
            why,
        };
        match asker {
            Asker::Grant(decision) => {
                for right in &new {
                    consent.set(right.clone(), *decision);
                }
            }
            Asker::Script(script) => {
                let mut left = Vec::new();
                for right in &new {
                    match script.decide(right) {
                        Some(decision) => consent.set(right.clone(), decision),
                        None => left.push(right.clone()),
                    }
                }
                if !left.is_empty() {
                    return Err(Unanswered {
                        rights: left,
                        why: Why::Nobody,
                    });
                }
            }
            Asker::Window { automation } => {
                // The window shows every right with what was decided before; its answer is the answer to all.
                let request = request_for(app, wanted, &consent, automation.clone());
                match ask_in_window(&request) {
                    Ok(Some(answers)) => {
                        for (right, decision) in answers {
                            consent.set(right, decision);
                        }
                    }
                    Ok(None) => return Err(unanswered(Why::Cancelled)),
                    Err(reason) => return Err(unanswered(Why::Failed(reason))),
                }
            }
            Asker::Nobody => return Err(unanswered(Why::Nobody)),
        }
    }
    consent.retain(wanted);
    Ok(Settled {
        consent,
        changed: !new.is_empty() || dropped,
    })
}

/// The rights of the running application are what the user decided, right by right. The following
/// of the store (`watch`) narrows whatever is too wide, so a wrong start would be put right behind
/// its back within a moment: the start is checked instead of being trusted to that.
pub fn enforce(permissions: &PermissionSet, decided: &Consent) -> Result<(), String> {
    let in_force = permissions.consent();
    for right in permissions.rights() {
        if in_force.decision(&right) != decided.decision(&right) {
            return Err(format!(
                "the decision for {right} is not the one the user made"
            ));
        }
    }
    Ok(())
}

/// How often a running application looks at the decisions of the user for a right taken back.
pub const WATCH_EVERY: Duration = Duration::from_millis(500);

/// Follows the store while the application runs: a decision that takes a right back (to a stand-in,
/// or away) applies at once, one that gives more waits for the next start (see `PermissionSet::narrow`).
pub fn watch(
    permissions: Arc<PermissionSet>,
    store: FileConsentStore,
    identity: Identity,
    every: Duration,
) -> tokio::task::JoinHandle<()> {
    tokio::spawn(async move {
        let mut tick = tokio::time::interval(every);
        loop {
            tick.tick().await;
            if let Ok(Some(stored)) = store.load(&identity) {
                permissions.narrow(&stored);
            }
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn app() -> AppSummary {
        AppSummary {
            id: "org.example.app".to_owned(),
            name: "Example".to_owned(),
            version: "1.0.0".to_owned(),
        }
    }

    fn rights() -> Vec<Right> {
        vec![
            Right::plain("clipboard.read"),
            Right::scoped("app.env", "HOME"),
            Right::scoped("net.http", "https://example.com/*"),
        ]
    }

    #[test]
    fn nothing_is_asked_when_everything_is_decided_and_nothing_is_written() {
        let wanted = rights();
        let mut stored = Consent::undecided();
        for (index, right) in wanted.iter().enumerate() {
            stored.set(
                right.clone(),
                [Decision::Allow, Decision::Substitute, Decision::Deny][index],
            );
        }
        let settled = settle(&app(), &wanted, Some(stored.clone()), &Asker::Nobody).unwrap();
        assert!(!settled.changed);
        assert_eq!(settled.consent, stored);
    }

    #[test]
    fn a_closed_manifest_needs_no_answer_and_leaves_no_file() {
        let settled = settle(&app(), &[], None, &Asker::Nobody).unwrap();
        assert!(!settled.changed);
    }

    #[test]
    fn new_rights_are_asked_and_the_old_decisions_are_kept() {
        let wanted = rights();
        let mut stored = Consent::undecided();
        stored.set(wanted[0].clone(), Decision::Deny);
        let settled = settle(
            &app(),
            &wanted,
            Some(stored),
            &Asker::Grant(Decision::Substitute),
        )
        .unwrap();
        assert!(settled.changed);
        assert_eq!(settled.consent.decision(&wanted[0]), Decision::Deny);
        assert_eq!(settled.consent.decision(&wanted[1]), Decision::Substitute);
        assert_eq!(settled.consent.decision(&wanted[2]), Decision::Substitute);
    }

    #[test]
    fn without_anybody_to_ask_the_rights_are_named_and_nothing_starts() {
        let wanted = rights();
        let mut stored = Consent::undecided();
        stored.set(wanted[1].clone(), Decision::Allow);
        let error = settle(&app(), &wanted, Some(stored), &Asker::Nobody).unwrap_err();
        assert_eq!(error.rights, [wanted[0].clone(), wanted[2].clone()]);
        let text = error.to_string();
        assert!(text.contains("clipboard.read, net.http:https://example.com/*"));
        assert!(text.contains("--grant"));
    }

    #[test]
    fn a_right_the_manifest_dropped_is_forgotten_and_the_store_is_rewritten() {
        let wanted = rights();
        let mut stored = Consent::undecided();
        for right in &wanted {
            stored.set(right.clone(), Decision::Allow);
        }
        let settled = settle(&app(), &wanted[..2], Some(stored), &Asker::Nobody).unwrap();
        assert!(settled.changed);
        assert!(!settled.consent.decided(&wanted[2]));
        assert!(settled.consent.decided(&wanted[1]));
    }

    #[test]
    fn a_script_names_rights_and_may_cover_the_rest() {
        let script = Script::parse(
            "clipboard.read=deny; app.env:HOME=substitute; net.http:https://x.example/?a=b=allow",
        )
        .unwrap();
        assert_eq!(
            script.decide(&Right::plain("clipboard.read")),
            Some(Decision::Deny)
        );
        assert_eq!(
            script.decide(&Right::scoped("app.env", "HOME")),
            Some(Decision::Substitute)
        );
        assert_eq!(
            script.decide(&Right::scoped("net.http", "https://x.example/?a=b")),
            Some(Decision::Allow)
        );
        assert_eq!(script.decide(&Right::plain("secrets")), None);
        let all = Script::parse("*=allow;clipboard.read=deny").unwrap();
        assert_eq!(all.decide(&Right::plain("secrets")), Some(Decision::Allow));
        assert_eq!(
            all.decide(&Right::plain("clipboard.read")),
            Some(Decision::Deny)
        );
        for bad in [
            "clipboard.read",
            "clipboard.read=maybe",
            "=allow",
            "x y=allow",
        ] {
            assert!(Script::parse(bad).is_err(), "{bad:?}");
        }
    }

    #[tokio::test]
    async fn a_right_taken_back_in_the_store_reaches_the_running_application_and_a_right_given_does_not(
    ) {
        use crate::{
            consent::fixtures::app,
            plan::{load_manifest, make_plan, path_vars},
        };
        use alef_core::security::{grants::Grants, permissions::Permission};
        let scratch = tempfile::tempdir().unwrap();
        let folder = app(scratch.path(), "org.example.app");
        let manifest = load_manifest(&folder).unwrap();
        let vars = path_vars(&folder, &manifest.id).unwrap();
        let plan = make_plan(&folder, manifest, &vars).unwrap();
        let mut started = Consent::undecided();
        started.set(Right::plain("clipboard.read"), Decision::Allow);
        started.set(Right::scoped("app.env", "HOME"), Decision::Deny);
        let permissions = Arc::new((*plan.permissions).clone().with_consent(started));
        let store = FileConsentStore::new(scratch.path().join("store"));
        let identity = identity_of(&folder, "org.example.app").unwrap();
        let handle = watch(
            permissions.clone(),
            store.clone(),
            identity.clone(),
            Duration::from_millis(10),
        );
        let mut changed = Consent::undecided();
        changed.set(Right::plain("clipboard.read"), Decision::Deny);
        changed.set(Right::scoped("app.env", "HOME"), Decision::Allow);
        store.save(&identity, &changed).unwrap();
        let grants = Grants::new();
        let mut waited = 0;
        while permissions
            .check(Permission::ClipboardRead, None, &grants)
            .is_ok()
        {
            assert!(
                waited < 400,
                "the right taken back never reached the application"
            );
            tokio::time::sleep(Duration::from_millis(10)).await;
            waited += 1;
        }
        assert!(
            permissions
                .check(Permission::AppEnv, Some("HOME"), &grants)
                .is_err(),
            "a right given waits for the next start"
        );
        handle.abort();
    }

    #[test]
    fn the_start_is_checked_against_what_the_user_decided() {
        use crate::{
            consent::fixtures::app,
            plan::{load_manifest, make_plan, path_vars},
        };
        let scratch = tempfile::tempdir().unwrap();
        let folder = app(scratch.path(), "org.example.app");
        let manifest = load_manifest(&folder).unwrap();
        let vars = path_vars(&folder, &manifest.id).unwrap();
        let plan = make_plan(&folder, manifest, &vars).unwrap();
        let mut decided = Consent::undecided();
        decided.set(Right::plain("clipboard.read"), Decision::Substitute);
        decided.set(Right::scoped("app.env", "HOME"), Decision::Deny);
        let right = (*plan.permissions).clone().with_consent(decided.clone());
        assert_eq!(enforce(&right, &decided), Ok(()));
        let too_wide = (*plan.permissions)
            .clone()
            .with_consent(Consent::allow_all());
        let error = enforce(&too_wide, &decided).unwrap_err();
        assert!(error.contains("not the one the user made"), "{error}");
        let mut other = decided.clone();
        other.set(Right::scoped("app.env", "HOME"), Decision::Allow);
        assert!(
            enforce(&right, &other).is_err(),
            "too narrow is wrong as well"
        );
    }

    #[test]
    fn one_folder_is_one_identity_and_another_folder_with_the_same_id_is_not() {
        let here = tempfile::tempdir().unwrap();
        let there = tempfile::tempdir().unwrap();
        let one = identity_of(here.path(), "org.example.app").unwrap();
        assert_eq!(one, identity_of(here.path(), "org.example.app").unwrap());
        assert_ne!(one, identity_of(there.path(), "org.example.app").unwrap());
        assert_ne!(one, identity_of(here.path(), "org.example.other").unwrap());
        assert_eq!(
            identity_of(&here.path().join("nowhere"), "x")
                .unwrap_err()
                .code,
            ErrorCode::NotFound
        );
    }
}
