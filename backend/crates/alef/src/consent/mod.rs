// SPDX-License-Identifier: MIT OR Apache-2.0
//! The launcher's side of consent: where the decisions of the user are kept, whom they belong to,
//! and how the rights an application asks for are decided before it starts.
use std::{
    fmt,
    num::NonZeroUsize,
    path::{Path, PathBuf},
    sync::{Arc, OnceLock},
    time::Duration,
};

use alef_core::{
    error::{AlefError, ErrorCode},
    security::{
        consent::{Consent, ConsentStore, Decision, FileConsentStore, Identity, Right},
        permissions::PermissionSet,
    },
    session::BlockingExecutor,
};
#[cfg(feature = "e2e")]
use serde_json::Value;
#[cfg(any(test, feature = "e2e"))]
use std::collections::BTreeMap;

pub mod ask;
#[cfg(feature = "e2e")]
mod hooks;
pub mod permissions;
#[cfg(feature = "e2e")]
use hooks::{scripted_asker, window_asker, with_automation};
#[cfg(test)]
pub(crate) mod fixtures;
#[cfg(test)]
mod settle_io_tests;

use ask::{ask_in_window, request_for, AppSummary};

/// End-to-end runs only (`ALEF_E2E=1`): the clicks of the user in the consent window, as JSON.
#[cfg(feature = "e2e")]
pub const UI_VARIABLE: &str = "ALEF_E2E_CONSENT_UI";

/// Moves the data folder of the runtime (decisions of the users, among others).
pub const HOME_VARIABLE: &str = "ALEF_HOME";

/// End-to-end runs only (`ALEF_E2E=1`): the answers to the questions, `right=decision;...`.
#[cfg(feature = "e2e")]
pub const SCRIPT_VARIABLE: &str = "ALEF_E2E_CONSENT";

/// The data folder of the runtime: nothing in it belongs to an application.
pub fn runtime_home() -> PathBuf {
    // macOS serves /tmp and /var through system symlinks and the store refuses linked
    // ancestors, so the configured root is resolved once here; links planted inside the
    // store stay refused.
    resolve_root(
        &std::env::var_os(HOME_VARIABLE)
            .map(PathBuf::from)
            .filter(|home| home.is_absolute())
            .unwrap_or_else(|| {
                dirs::data_local_dir()
                    .or_else(|| dirs::home_dir().map(|home| home.join(".local").join("share")))
                    .unwrap_or_else(std::env::temp_dir)
                    .join("alef")
            }),
    )
}

/// Canonicalizes the longest existing ancestor and re-attaches the missing tail, so a configured
/// root behind a system symlink (macOS `/var` -> `/private/var`) passes the store checks. Only
/// roots the user configures go through here; links below a root stay refused.
pub(crate) fn resolve_root(path: &Path) -> PathBuf {
    if cfg!(not(unix)) {
        return path.to_owned();
    }
    let mut tail: Vec<std::ffi::OsString> = Vec::new();
    let mut candidate = path.to_owned();
    loop {
        match std::fs::canonicalize(&candidate) {
            Ok(real) => {
                let mut resolved = real;
                for part in tail.iter().rev() {
                    resolved.push(part);
                }
                return resolved;
            }
            Err(_) => match (
                candidate.file_name().map(|name| name.to_owned()),
                candidate.parent(),
            ) {
                (Some(name), Some(parent)) => {
                    tail.push(name);
                    candidate = parent.to_owned();
                }
                _ => return path.to_owned(),
            },
        }
    }
}

/// Where the stand-ins of the application keep their content: a folder of the runtime under the
/// key of the identity, so that another folder or another key does not find it.
pub fn shadow_folder(identity: &Identity) -> PathBuf {
    runtime_home().join("shadow").join(identity.key())
}

/// Where the user keeps the AI usage of the application: the daily limits, the counters and the
/// journal.
pub fn ai_folder(identity: &Identity) -> PathBuf {
    runtime_home().join("ai").join(identity.key())
}

/// The decisions of the users, one file per application.
pub fn store() -> FileConsentStore {
    FileConsentStore::new(runtime_home().join("consent"))
}

/// Local consent budget of the launcher: at most two queued or running jobs — the reads of the
/// watch and the answer write of the consent window. This is not a global or per-session
/// framework limit.
pub(crate) fn executor() -> &'static BlockingExecutor {
    static EXECUTOR: OnceLock<BlockingExecutor> = OnceLock::new();
    EXECUTOR.get_or_init(|| {
        BlockingExecutor::new(NonZeroUsize::new(2).expect("non-zero consent budget"))
    })
}

/// The stored decisions of the application, read on `executor`: the file of the store is blocking
/// work and never a wait on the async context. A lost turn of the executor is a store failure.
pub async fn stored_decisions(
    executor: &BlockingExecutor,
    store: &FileConsentStore,
    identity: &Identity,
) -> std::io::Result<Option<Consent>> {
    let (store, identity) = (store.clone(), identity.clone());
    executor
        .submit(move || store.load(&identity))
        .map_err(store_error)?
        .result()
        .await
        .map_err(store_error)?
}

/// Writes the decisions of the application on `executor`; otherwise as [`stored_decisions`].
pub async fn store_decisions(
    executor: &BlockingExecutor,
    store: &FileConsentStore,
    identity: &Identity,
    consent: &Consent,
) -> std::io::Result<()> {
    let (store, identity, consent) = (store.clone(), identity.clone(), consent.clone());
    executor
        .submit(move || store.save(&identity, &consent))
        .map_err(store_error)?
        .result()
        .await
        .map_err(store_error)?
}

/// A lost turn on the consent budget (full, cancelled, panicked) is a store failure.
fn store_error(error: AlefError) -> std::io::Error {
    std::io::Error::other(error.to_string())
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
#[cfg(any(test, feature = "e2e"))]
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

#[cfg(any(test, feature = "e2e"))]
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
    #[cfg(any(test, feature = "e2e"))]
    Script(Script),
    /// The user, in the consent window.
    Window {
        #[cfg(feature = "e2e")]
        automation: Option<Value>,
    },
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
        if let Some(script) = scripted_asker()? {
            return Ok(script);
        }
        if no_prompt || !can_show_a_window() {
            return Ok(Self::Nobody);
        }
        window_asker()
    }
}

#[cfg(not(feature = "e2e"))]
fn scripted_asker() -> Result<Option<Asker>, String> {
    Ok(None)
}

#[cfg(not(feature = "e2e"))]
fn window_asker() -> Result<Asker, String> {
    Ok(Asker::Window {})
}

fn window_request(
    asker: &Asker,
    app: &AppSummary,
    wanted: &[Right],
    describe: &dyn Fn(&Right) -> Option<String>,
    consent: &Consent,
) -> ask::Request {
    let request = request_for(app, wanted, describe, consent);
    with_automation(request, asker)
}

#[cfg(not(feature = "e2e"))]
fn with_automation(request: ask::Request, _: &Asker) -> ask::Request {
    request
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
    settle_with(app, wanted, &|_| None, stored, asker)
}

/// [`settle`] where the window shows `describe(right)` next to a right (see
/// `PermissionSet::describe`).
pub fn settle_with(
    app: &AppSummary,
    wanted: &[Right],
    describe: &dyn Fn(&Right) -> Option<String>,
    stored: Option<Consent>,
    asker: &Asker,
) -> Result<Settled, Unanswered> {
    let (mut consent, new, dropped) = pending(stored, wanted);
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
            #[cfg(any(test, feature = "e2e"))]
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
            Asker::Window { .. } => {
                // The window shows every right with what was decided before; its answer is the answer to all.
                let request = window_request(asker, app, wanted, describe, &consent);
                match ask_in_window(&request) {
                    Ok(Some((decisions, _))) => {
                        for (right, decision) in decisions {
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

/// [`settle_with`] off the calling context: the ask of the window — the files of the exchange and
/// the wait for the window process — runs on `executor`, the other askers decide in place as
/// [`settle_with`] does. The outcomes and the persistence are the same.
pub async fn settle_with_async(
    executor: &BlockingExecutor,
    app: &AppSummary,
    wanted: &[Right],
    describe: &dyn Fn(&Right) -> Option<String>,
    stored: Option<Consent>,
    asker: &Asker,
) -> Result<Settled, Unanswered> {
    if !matches!(asker, Asker::Window { .. }) {
        return settle_with(app, wanted, describe, stored, asker);
    }
    let (mut consent, new, dropped) = pending(stored, wanted);
    if new.is_empty() {
        consent.retain(wanted);
        return Ok(Settled {
            consent,
            changed: dropped,
        });
    }
    // The question is built here; only owned data crosses into the executor.
    let request = window_request(asker, app, wanted, describe, &consent);
    let unanswered = |why| Unanswered {
        rights: new.clone(),
        why,
    };
    match ask::ask_in_window_on(executor, &request).await {
        Ok(Some((decisions, _))) => {
            for (right, decision) in decisions {
                consent.set(right, decision);
            }
        }
        Ok(None) => return Err(unanswered(Why::Cancelled)),
        Err(reason) => return Err(unanswered(Why::Failed(reason))),
    }
    consent.retain(wanted);
    Ok(Settled {
        consent,
        changed: true,
    })
}

/// What is undecided before any question: the consent to start from, the rights nobody decided on
/// and whether a right the manifest dropped is to be forgotten.
fn pending(stored: Option<Consent>, wanted: &[Right]) -> (Consent, Vec<Right>, bool) {
    let mut consent = stored.unwrap_or_else(Consent::undecided);
    let new: Vec<Right> = consent.missing(wanted).into_iter().cloned().collect();
    let dropped = consent
        .decisions()
        .any(|(right, _)| !wanted.contains(right));
    (consent, new, dropped)
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
            // The read is blocking file work: it waits on the consent budget, never on a runtime
            // worker; a tick that finds the budget full is skipped, the next one reads again.
            let Ok(job) = executor().submit({
                let (permissions, store, identity) =
                    (permissions.clone(), store.clone(), identity.clone());
                move || {
                    store
                        .load(&identity)
                        .ok()
                        .flatten()
                        .map(|stored| permissions.narrow(&stored))
                }
            }) else {
                continue;
            };
            let _ = job.result().await;
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Every await of a test is bounded: a regression fails instead of hanging the suite.
    async fn bounded<F: std::future::Future>(future: F) -> F::Output {
        tokio::time::timeout(std::time::Duration::from_secs(10), future)
            .await
            .expect("bounded test await")
    }

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

    #[cfg(not(feature = "e2e"))]
    #[test]
    fn feature_off_has_no_scripted_or_automated_asker() {
        assert_eq!(scripted_asker().unwrap(), None);
        assert_eq!(window_asker().unwrap(), Asker::Window {});
        assert_eq!(Asker::from_environment(None, true).unwrap(), Asker::Nobody);
    }

    #[test]
    fn an_explicit_grant_wins_even_without_a_prompt() {
        for decision in [Decision::Allow, Decision::Substitute, Decision::Deny] {
            assert_eq!(
                Asker::from_environment(Some(decision), true).unwrap(),
                Asker::Grant(decision)
            );
        }
    }

    #[test]
    fn a_script_settles_named_rights_and_refuses_unanswered_ones() {
        let wanted = rights();
        let script = Asker::Script(Script::parse("clipboard.read=deny;*=substitute").unwrap());
        let settled = settle(&app(), &wanted, None, &script).unwrap();
        assert_eq!(settled.consent.decision(&wanted[0]), Decision::Deny);
        assert_eq!(settled.consent.decision(&wanted[1]), Decision::Substitute);
        let partial = Asker::Script(Script::parse("clipboard.read=deny").unwrap());
        assert_eq!(
            settle(&app(), &wanted, None, &partial).unwrap_err().rights,
            wanted[1..]
        );
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

    fn window_asker_of_tests() -> Asker {
        #[cfg(feature = "e2e")]
        let asker = Asker::Window { automation: None };
        #[cfg(not(feature = "e2e"))]
        let asker = Asker::Window {};
        asker
    }

    #[tokio::test]
    async fn askers_that_decide_in_place_settle_the_same_through_the_async_path() {
        let executor = BlockingExecutor::new(std::num::NonZeroUsize::new(2).unwrap());
        let wanted = rights();
        let granted = bounded(settle_with_async(
            &executor,
            &app(),
            &wanted,
            &|_| None,
            None,
            &Asker::Grant(Decision::Allow),
        ))
        .await
        .unwrap();
        let settled = settle(&app(), &wanted, None, &Asker::Grant(Decision::Allow)).unwrap();
        assert_eq!(granted.consent, settled.consent);
        assert_eq!(granted.changed, settled.changed);
        let async_error = bounded(settle_with_async(
            &executor,
            &app(),
            &wanted,
            &|_| None,
            None,
            &Asker::Nobody,
        ))
        .await
        .unwrap_err();
        assert_eq!(
            async_error,
            settle(&app(), &wanted, None, &Asker::Nobody).unwrap_err()
        );
    }

    #[tokio::test]
    async fn nothing_is_asked_off_the_bus_when_every_right_is_already_decided() {
        let wanted = rights();
        let mut stored = Consent::undecided();
        for (index, right) in wanted.iter().enumerate() {
            stored.set(
                right.clone(),
                [Decision::Allow, Decision::Substitute, Decision::Deny][index],
            );
        }
        let executor = BlockingExecutor::new(std::num::NonZeroUsize::new(2).unwrap());
        let asker = window_asker_of_tests();
        let settled = bounded(settle_with_async(
            &executor,
            &app(),
            &wanted,
            &|_| None,
            Some(stored.clone()),
            &asker,
        ))
        .await
        .unwrap();
        assert!(!settled.changed);
        assert_eq!(settled.consent, stored);
    }

    #[tokio::test]
    async fn the_decisions_are_written_and_read_on_an_injected_executor() {
        let executor = BlockingExecutor::new(std::num::NonZeroUsize::new(2).unwrap());
        let scratch = tempfile::tempdir().unwrap();
        let store = FileConsentStore::new(scratch.path().join("consent"));
        let identity = Identity {
            app_id: "org.example.app".to_owned(),
            signer: Some("dir:example".to_owned()),
        };
        let nothing = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            stored_decisions(&executor, &store, &identity),
        )
        .await
        .expect("the read finishes")
        .unwrap();
        assert_eq!(nothing, None, "nothing is stored before the first write");
        let mut consent = Consent::undecided();
        consent.set(Right::plain("clipboard.read"), Decision::Substitute);
        tokio::time::timeout(
            std::time::Duration::from_secs(5),
            store_decisions(&executor, &store, &identity, &consent),
        )
        .await
        .expect("the write finishes")
        .unwrap();
        let read_back = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            stored_decisions(&executor, &store, &identity),
        )
        .await
        .expect("the read finishes")
        .unwrap();
        assert_eq!(read_back, Some(consent));
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

    #[cfg(unix)]
    mod home_links {
        use super::*;
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(0);

        /// A real directory and a symlink to it, both unique to this test.
        fn linked_root() -> (PathBuf, PathBuf) {
            let n = NEXT.fetch_add(1, Ordering::Relaxed);
            let target =
                std::env::temp_dir().join(format!("alef-root-target-{}-{n}", std::process::id()));
            std::fs::create_dir_all(&target).expect("create target");
            let link =
                std::env::temp_dir().join(format!("alef-root-link-{}-{n}", std::process::id()));
            std::os::unix::fs::symlink(&target, &link).expect("symlink");
            (target, link)
        }

        #[test]
        fn a_symlinked_root_resolves_to_its_target_and_keeps_the_missing_tail() {
            let (target, link) = linked_root();
            let real = std::fs::canonicalize(&target).unwrap();
            assert_eq!(resolve_root(&link), real);
            assert_eq!(
                resolve_root(&link.join("alef").join("apps")),
                real.join("alef").join("apps")
            );
            let _ = std::fs::remove_file(&link);
            let _ = std::fs::remove_dir_all(&target);
        }

        #[test]
        fn a_resolved_symlinked_root_passes_the_store_prefix_check_but_the_raw_link_does_not() {
            let (target, link) = linked_root();
            assert!(crate::install::store::create_checked_dir(&link.join("apps")).is_err());
            crate::install::store::create_checked_dir(&resolve_root(&link).join("apps"))
                .expect("store root under a resolved linked home");
            let _ = std::fs::remove_file(&link);
            let _ = std::fs::remove_dir_all(&target);
        }
    }
}
