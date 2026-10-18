// SPDX-License-Identifier: MIT OR Apache-2.0
//! The question to the user and the answer, as the launcher and the consent window pass them to each
//! other. The window runs in a process of its own (`alef consent <REQUEST> <ANSWER>`): a window
//! system takes one event loop per process, and the application has not started yet.
use std::{
    collections::BTreeMap,
    fs, io,
    path::{Path, PathBuf},
    process::Command,
};

use alef_core::{
    security::consent::{Consent, Decision, Right, RiskLevel},
    session::BlockingExecutor,
};
use serde::{Deserialize, Serialize};
#[cfg(feature = "e2e")]
use serde_json::Value;

use crate::consent::runtime_home;

/// Who asks, as the window says it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppSummary {
    pub id: String,
    pub name: String,
    pub version: String,
}

/// One right in the window.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AskedRight {
    pub permission: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    /// How seriously the user should regard the right.
    #[serde(default)]
    pub level: RiskLevel,
    /// Why the user should think twice (see `Right::risk`); allowing it needs a confirmation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub risk: Option<String>,
    /// What the manifest says about the right (a declared command: its description and command
    /// line); text of the application.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
    /// What the user decided before, if he did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub decision: Option<Decision>,
}

impl AskedRight {
    fn right(&self) -> Right {
        Right {
            permission: self.permission.clone(),
            scope: self.scope.clone(),
        }
    }
}

/// The trust question next to the rights, when the application comes from a package.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TrustAsk {
    /// The full fingerprint of the signer; `None` for an unsigned package.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<String>,
    /// The SHA-256 digest of the exact archive bytes of an unsigned package.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archive_digest: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub app: AppSummary,
    pub rights: Vec<AskedRight>,
    /// The trust of the package signer, next to the rights.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trust: Option<TrustAsk>,
    /// End-to-end runs only: the clicks of a user, played by the page.
    #[cfg(feature = "e2e")]
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub automation: Option<Value>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Answered {
    pub permission: String,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub scope: Option<String>,
    pub decision: Decision,
    /// The user confirmed that he understands the risk of a risky right he allowed.
    #[serde(default)]
    pub confirmed: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Answer {
    pub decisions: Vec<Answered>,
    /// Whether the trust question was accepted, when it was asked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trust: Option<bool>,
}

/// The question for every right the manifest asks, with what was decided before; `describe` gives
/// the detail a right is shown with.
pub fn request_for(
    app: &AppSummary,
    wanted: &[Right],
    describe: &dyn Fn(&Right) -> Option<String>,
    stored: &Consent,
) -> Request {
    Request {
        app: app.clone(),
        rights: wanted
            .iter()
            .map(|right| AskedRight {
                permission: right.permission.clone(),
                scope: right.scope.clone(),
                level: right.level(),
                risk: right.risk().map(str::to_owned),
                detail: describe(right),
                decision: stored.decided(right).then(|| stored.decision(right)),
            })
            .collect(),
        trust: None,
        #[cfg(feature = "e2e")]
        automation: None,
    }
}

/// Believes an answer only if it is exactly the answer to the question: every right once, none
/// that was not asked, and a risky right allowed only with its confirmation. The page is a stranger
/// to this check: the window is the user's, but what it sends is not trusted to be whole.
pub fn validate(
    request: &Request,
    answer: &Answer,
) -> Result<(BTreeMap<Right, Decision>, Option<bool>), String> {
    let mut decided = BTreeMap::new();
    for given in &answer.decisions {
        let right = Right {
            permission: given.permission.clone(),
            scope: given.scope.clone(),
        };
        let asked = request
            .rights
            .iter()
            .find(|asked| asked.right() == right)
            .ok_or_else(|| format!("{right} was not asked"))?;
        if asked.risk.is_some() && given.decision == Decision::Allow && !given.confirmed {
            return Err(format!(
                "{right} is risky: allowing it needs the confirmation"
            ));
        }
        if decided.insert(right.clone(), given.decision).is_some() {
            return Err(format!("{right} is answered twice"));
        }
    }
    for asked in &request.rights {
        if !decided.contains_key(&asked.right()) {
            return Err(format!("{} is not answered", asked.right()));
        }
    }
    match (request.trust.as_ref(), answer.trust) {
        (Some(_), None) => return Err("the trust question is not answered".to_owned()),
        (None, Some(_)) => return Err("trust was not asked".to_owned()),
        _ => {}
    }
    Ok((decided, answer.trust))
}

/// Where the files that pass the question and the answer lie: in the folder of the runtime, which
/// no application reaches.
pub fn exchange_folder() -> PathBuf {
    runtime_home().join("ask")
}

fn io_text(what: &str, error: io::Error) -> String {
    format!("{what}: {error}")
}

/// Writes the answer so that the other process never reads half of it.
pub fn write_answer(path: &Path, answer: &Answer) -> io::Result<()> {
    let partial = path.with_extension("part");
    fs::write(
        &partial,
        serde_json::to_vec(answer).map_err(io::Error::other)?,
    )?;
    fs::rename(&partial, path)
}

/// [`ask_in_window`] on `executor`: the files of the exchange are written and the window process
/// is waited for off the async context. A lost turn of the executor (full, cancelled) is a
/// failure of the window.
pub async fn ask_in_window_on(
    executor: &BlockingExecutor,
    request: &Request,
) -> Result<Option<(BTreeMap<Right, Decision>, Option<bool>)>, String> {
    let request = request.clone();
    executor
        .submit(move || ask_in_window(&request))
        .map_err(|error| format!("the window cannot be started: {error}"))?
        .result()
        .await
        .map_err(|error| format!("the window cannot be waited for: {error}"))?
}

/// [`write_answer`] on `executor`: the file of the exchange is blocking work, and the handler of
/// the window must not hold a runtime worker while it writes.
pub async fn write_answer_on(
    executor: &BlockingExecutor,
    path: &Path,
    answer: &Answer,
) -> io::Result<()> {
    let (path, answer) = (path.to_owned(), answer.clone());
    executor
        .submit(move || write_answer(&path, &answer))
        .map_err(|error| io::Error::other(format!("the answer cannot be written: {error}")))?
        .result()
        .await
        .map_err(|error| io::Error::other(format!("the answer cannot be written: {error}")))?
}

/// [`write_answer_on`] on the shared consent budget.
pub async fn write_answer_async(path: &Path, answer: &Answer) -> io::Result<()> {
    write_answer_on(super::executor(), path, answer).await
}

/// Shows the window in a process of its own and waits for it. `Ok(None)`: the user closed it
/// without answering. The files of the exchange are removed whatever happens.
pub fn ask_in_window(
    request: &Request,
) -> Result<Option<(BTreeMap<Right, Decision>, Option<bool>)>, String> {
    let folder = exchange_folder();
    fs::create_dir_all(&folder).map_err(|e| io_text("the question cannot be put down", e))?;
    let stem = format!("{}", std::process::id());
    let request_path = folder.join(format!("{stem}.request.json"));
    let answer_path = folder.join(format!("{stem}.answer.json"));
    let _ = fs::remove_file(&answer_path);
    let outcome = (|| {
        fs::write(
            &request_path,
            serde_json::to_vec(request).map_err(|e| e.to_string())?,
        )
        .map_err(|e| io_text("the question cannot be put down", e))?;
        let exe = std::env::current_exe().map_err(|e| io_text("the runtime cannot be found", e))?;
        let status = Command::new(exe)
            .arg("consent")
            .arg(&request_path)
            .arg(&answer_path)
            .status()
            .map_err(|e| io_text("the window cannot be started", e))?;
        let bytes = match fs::read(&answer_path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == io::ErrorKind::NotFound => {
                return if status.success() {
                    Ok(None)
                } else {
                    Err(format!("the window ended with {status}"))
                };
            }
            Err(error) => return Err(io_text("the answer cannot be read", error)),
        };
        let answer: Answer =
            serde_json::from_slice(&bytes).map_err(|e| format!("the answer is damaged: {e}"))?;
        validate(request, &answer).map(Some)
    })();
    let _ = fs::remove_file(&request_path);
    let _ = fs::remove_file(&answer_path);
    outcome
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::Future;

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
            Right::scoped("cli.exec", "*"),
            Right::scoped("app.env", "HOME"),
        ]
    }

    fn answer(items: &[(&Right, Decision, bool)]) -> Answer {
        Answer {
            decisions: items
                .iter()
                .map(|(right, decision, confirmed)| Answered {
                    permission: right.permission.clone(),
                    scope: right.scope.clone(),
                    decision: *decision,
                    confirmed: *confirmed,
                })
                .collect(),
            trust: None,
        }
    }

    #[cfg(not(feature = "e2e"))]
    #[test]
    fn feature_off_discards_automation_from_the_question() {
        let request = request_for(&app(), &rights(), &|_| None, &Consent::undecided());
        let mut wire = serde_json::to_value(&request).unwrap();
        wire["automation"] = serde_json::json!([{ "submit": true }]);
        let decoded: Request = serde_json::from_value(wire).unwrap();
        assert_eq!(decoded, request);
        assert!(!serde_json::to_string(&decoded)
            .unwrap()
            .contains("automation"));
    }

    #[test]
    fn the_question_names_every_right_marks_the_risky_ones_and_shows_what_was_decided() {
        let wanted = rights();
        let mut stored = Consent::undecided();
        stored.set(wanted[2].clone(), Decision::Substitute);
        let request = request_for(&app(), &wanted, &|_| None, &stored);
        assert_eq!(request.rights.len(), 3);
        assert_eq!(request.rights[0].risk, None);
        assert!(
            request.rights[1].risk.is_some(),
            "a program of any name is risky"
        );
        assert_eq!(request.rights[2].decision, Some(Decision::Substitute));
        assert_eq!(request.rights[0].decision, None);
        let text = serde_json::to_string(&request).unwrap();
        assert!(!text.contains("automation"), "{text}");
        assert!(!text.contains("trust"), "{text}");
        assert!(text.contains("\"permission\":\"clipboard.read\""));
    }

    #[test]
    fn tray_is_a_plain_non_risky_right_with_all_three_answers() {
        let right = Right::plain("tray");
        let request = request_for(
            &app(),
            std::slice::from_ref(&right),
            &|_| None,
            &Consent::undecided(),
        );
        assert_eq!(request.rights[0].permission, "tray");
        assert_eq!(request.rights[0].scope, None);
        assert_eq!(request.rights[0].risk, None);
        let page = include_str!("../../assets/consent/consent.js");
        assert!(page.contains("['tray', 'Tray icon']"));
        assert!(page.contains("['ai', 'AI']"));
        for decision in [Decision::Allow, Decision::Substitute, Decision::Deny] {
            let answered = answer(&[(&right, decision, false)]);
            assert_eq!(validate(&request, &answered).unwrap().0[&right], decision);
        }
    }

    #[test]
    fn a_red_right_keeps_its_warning_and_requires_confirmation_to_allow() {
        let right = Right::plain("ai.builtin.bash");
        let request = request_for(
            &app(),
            std::slice::from_ref(&right),
            &|_| None,
            &Consent::undecided(),
        );
        let asked = &request.rights[0];
        assert_eq!(asked.level, RiskLevel::Red);
        assert!(asked
            .risk
            .as_deref()
            .unwrap()
            .contains("bypasses all protections, including the AI model keys"));

        let missing_confirmation = answer(&[(&right, Decision::Allow, false)]);
        assert!(validate(&request, &missing_confirmation)
            .unwrap_err()
            .contains("needs the confirmation"));
        let confirmed = answer(&[(&right, Decision::Allow, true)]);
        assert_eq!(
            validate(&request, &confirmed).unwrap().0[&right],
            Decision::Allow
        );
    }

    #[test]
    fn a_right_is_shown_with_the_detail_the_manifest_gives_it() {
        let wanted = [
            Right::scoped("cli.command", "status"),
            Right::plain("clipboard.read"),
        ];
        let describe = |right: &Right| {
            (right.permission == "cli.command").then(|| "status — Shows (git status)".to_owned())
        };
        let request = request_for(&app(), &wanted, &describe, &Consent::undecided());
        assert_eq!(
            request.rights[0].detail.as_deref(),
            Some("status — Shows (git status)")
        );
        assert_eq!(request.rights[1].detail, None);
        let text = serde_json::to_string(&request).unwrap();
        assert_eq!(text.matches("detail").count(), 1, "{text}");
    }

    #[test]
    fn only_the_whole_answer_is_believed() {
        let wanted = rights();
        let request = request_for(&app(), &wanted, &|_| None, &Consent::undecided());
        let whole = answer(&[
            (&wanted[0], Decision::Substitute, false),
            (&wanted[1], Decision::Allow, true),
            (&wanted[2], Decision::Deny, false),
        ]);
        let decided = validate(&request, &whole).unwrap().0;
        assert_eq!(decided[&wanted[0]], Decision::Substitute);
        assert_eq!(decided[&wanted[1]], Decision::Allow);
        assert_eq!(decided[&wanted[2]], Decision::Deny);

        let missing = answer(&[(&wanted[0], Decision::Allow, false)]);
        assert!(validate(&request, &missing)
            .unwrap_err()
            .contains("is not answered"));

        let stray = Right::plain("secrets");
        let mut extra = whole.clone();
        extra
            .decisions
            .extend(answer(&[(&stray, Decision::Allow, false)]).decisions);
        assert!(validate(&request, &extra)
            .unwrap_err()
            .contains("was not asked"));

        let mut twice = whole.clone();
        twice.decisions.push(twice.decisions[0].clone());
        assert!(validate(&request, &twice)
            .unwrap_err()
            .contains("answered twice"));

        let unconfirmed = answer(&[
            (&wanted[0], Decision::Allow, false),
            (&wanted[1], Decision::Allow, false),
            (&wanted[2], Decision::Allow, false),
        ]);
        assert!(validate(&request, &unconfirmed)
            .unwrap_err()
            .contains("needs the confirmation"));
        let softer = answer(&[
            (&wanted[0], Decision::Allow, false),
            (&wanted[1], Decision::Substitute, false),
            (&wanted[2], Decision::Allow, false),
        ]);
        assert!(
            validate(&request, &softer).is_ok(),
            "a risky right that is not allowed needs no confirmation"
        );
    }

    #[test]
    fn an_old_answer_without_trust_stays_valid_and_a_stray_trust_answer_is_refused() {
        let wanted = rights();
        let request = request_for(&app(), &wanted, &|_| None, &Consent::undecided());
        let given = answer(&[
            (&wanted[0], Decision::Allow, false),
            (&wanted[1], Decision::Substitute, false),
            (&wanted[2], Decision::Allow, false),
        ]);
        let (decided, trust) = validate(&request, &given).unwrap();
        assert_eq!(trust, None);
        assert_eq!(decided.len(), wanted.len());

        // A trust answer to a question that did not ask trust is refused, the rights being complete.
        let mut stray = answer(&[
            (&wanted[0], Decision::Allow, false),
            (&wanted[1], Decision::Substitute, false),
            (&wanted[2], Decision::Allow, false),
        ]);
        stray.trust = Some(true);
        assert_eq!(
            validate(&request, &stray).unwrap_err(),
            "trust was not asked"
        );
        // The rights are checked first: an answer without them is refused for them.
        let incomplete = Answer {
            decisions: Vec::new(),
            trust: Some(true),
        };
        assert_eq!(
            validate(&request, &incomplete).unwrap_err(),
            "clipboard.read is not answered"
        );
    }

    #[tokio::test]
    async fn the_answer_is_written_through_an_injected_executor_and_is_never_half_written() {
        let executor = BlockingExecutor::new(std::num::NonZeroUsize::new(2).unwrap());
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("a.answer.json");
        let wanted = rights();
        let given = answer(&[(&wanted[0], Decision::Deny, false)]);
        tokio::time::timeout(
            std::time::Duration::from_secs(10),
            write_answer_on(&executor, &path, &given),
        )
        .await
        .expect("the answer is written in time")
        .unwrap();
        let back: Answer = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(back, given);
        assert!(!path.with_extension("part").exists());
    }

    #[tokio::test]
    async fn a_full_budget_refuses_the_window_without_starting_it() {
        let executor = BlockingExecutor::new(std::num::NonZeroUsize::new(1).unwrap());
        let (started_tx, started_rx) = tokio::sync::oneshot::channel();
        let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
        let blocker = executor
            .submit(move || {
                let _ = started_tx.send(());
                let _ = release_rx.recv_timeout(std::time::Duration::from_secs(30));
            })
            .unwrap();
        tokio::time::timeout(std::time::Duration::from_secs(5), started_rx)
            .await
            .expect("blocker started")
            .expect("signal");
        let request = request_for(&app(), &rights(), &|_| None, &Consent::undecided());
        let error = tokio::time::timeout(
            std::time::Duration::from_secs(5),
            ask_in_window_on(&executor, &request),
        )
        .await
        .expect("the refused ask does not wait")
        .unwrap_err();
        assert!(error.contains("full"), "{error}");
        release_tx.send(()).expect("release the blocker");
        tokio::time::timeout(std::time::Duration::from_secs(5), blocker.result())
            .await
            .expect("blocker finishes")
            .expect("blocker job");
    }

    #[test]
    fn the_answer_write_waits_for_its_turn_on_the_executor_and_does_not_run_inline() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .max_blocking_threads(1)
            .enable_all()
            .build()
            .expect("the test runtime starts");
        runtime.block_on(async {
            let executor = BlockingExecutor::new(std::num::NonZeroUsize::new(2).unwrap());
            // The single blocking thread of the runtime is held by the blocker, so the submitted
            // write is admitted but cannot start: a poll stays pending and no file appears.
            let (started_tx, started_rx) = tokio::sync::oneshot::channel();
            let (release_tx, release_rx) = std::sync::mpsc::channel::<()>();
            let blocker = tokio::task::spawn_blocking(move || {
                let _ = started_tx.send(());
                let _ = release_rx.recv_timeout(std::time::Duration::from_secs(30));
            });
            tokio::time::timeout(std::time::Duration::from_secs(5), started_rx)
                .await
                .expect("blocker started")
                .expect("signal");
            let folder = tempfile::tempdir().unwrap();
            let path = folder.path().join("a.answer.json");
            let wanted = rights();
            let given = answer(&[(&wanted[0], Decision::Deny, false)]);
            let mut write = Box::pin(write_answer_on(&executor, &path, &given));
            let waker = std::task::Waker::noop();
            let mut cx = std::task::Context::from_waker(waker);
            assert!(
                write.as_mut().poll(&mut cx).is_pending(),
                "an inline write would finish in the first poll"
            );
            assert!(!path.exists(), "the answer was written before its turn");
            release_tx.send(()).expect("release the blocker");
            tokio::time::timeout(std::time::Duration::from_secs(5), write.as_mut())
                .await
                .expect("the write finishes after the release")
                .expect("the write succeeds");
            let back: Answer = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
            assert_eq!(back, given);
            tokio::time::timeout(std::time::Duration::from_secs(5), blocker)
                .await
                .expect("blocker exits")
                .expect("blocker joins");
        });
    }

    #[test]
    fn a_trust_question_with_zero_rights_is_answered_by_the_trust_field_alone() {
        let fp = "a".repeat(64);
        let request = Request {
            app: app(),
            rights: Vec::new(),
            trust: Some(TrustAsk {
                fingerprint: Some(fp),
                archive_digest: None,
            }),
            #[cfg(feature = "e2e")]
            automation: None,
        };
        let (decided, trust) = validate(
            &request,
            &Answer {
                decisions: Vec::new(),
                trust: Some(true),
            },
        )
        .unwrap();
        assert_eq!(decided, BTreeMap::new());
        assert_eq!(trust, Some(true));

        let silent = Answer {
            decisions: Vec::new(),
            trust: None,
        };
        assert_eq!(
            validate(&request, &silent).unwrap_err(),
            "the trust question is not answered"
        );

        let stray = Answer {
            decisions: vec![Answered {
                permission: "clipboard.read".to_owned(),
                scope: None,
                decision: Decision::Allow,
                confirmed: false,
            }],
            trust: Some(false),
        };
        assert!(validate(&request, &stray)
            .unwrap_err()
            .contains("was not asked"));
    }

    #[test]
    fn an_answer_travels_as_a_file_that_is_never_half_written() {
        let folder = tempfile::tempdir().unwrap();
        let path = folder.path().join("a.answer.json");
        let wanted = rights();
        let given = answer(&[(&wanted[0], Decision::Deny, false)]);
        write_answer(&path, &given).unwrap();
        let back: Answer = serde_json::from_slice(&fs::read(&path).unwrap()).unwrap();
        assert_eq!(back, given);
        assert!(!path.with_extension("part").exists());
    }
}
