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

use alef_core::security::consent::{Consent, Decision, Right};
use serde::{Deserialize, Serialize};
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
    /// Why the user should think twice (see `Right::risk`); allowing it needs a confirmation.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub risk: Option<String>,
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

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Request {
    pub app: AppSummary,
    pub rights: Vec<AskedRight>,
    /// End-to-end runs only: the clicks of a user, played by the page.
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
}

/// The question for every right the manifest asks, with what was decided before.
pub fn request_for(
    app: &AppSummary,
    wanted: &[Right],
    stored: &Consent,
    automation: Option<Value>,
) -> Request {
    Request {
        app: app.clone(),
        rights: wanted
            .iter()
            .map(|right| AskedRight {
                permission: right.permission.clone(),
                scope: right.scope.clone(),
                risk: right.risk().map(str::to_owned),
                decision: stored.decided(right).then(|| stored.decision(right)),
            })
            .collect(),
        automation,
    }
}

/// Believes an answer only if it is exactly the answer to the question: every right once, none
/// that was not asked, and a risky right allowed only with its confirmation. The page is a stranger
/// to this check: the window is the user's, but what it sends is not trusted to be whole.
pub fn validate(request: &Request, answer: &Answer) -> Result<BTreeMap<Right, Decision>, String> {
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
    Ok(decided)
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

/// Shows the window in a process of its own and waits for it. `Ok(None)`: the user closed it
/// without answering. The files of the exchange are removed whatever happens.
pub fn ask_in_window(request: &Request) -> Result<Option<BTreeMap<Right, Decision>>, String> {
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
        }
    }

    #[test]
    fn the_question_names_every_right_marks_the_risky_ones_and_shows_what_was_decided() {
        let wanted = rights();
        let mut stored = Consent::undecided();
        stored.set(wanted[2].clone(), Decision::Substitute);
        let request = request_for(&app(), &wanted, &stored, None);
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
        assert!(text.contains("\"permission\":\"clipboard.read\""));
    }

    #[test]
    fn only_the_whole_answer_is_believed() {
        let wanted = rights();
        let request = request_for(&app(), &wanted, &Consent::undecided(), None);
        let whole = answer(&[
            (&wanted[0], Decision::Substitute, false),
            (&wanted[1], Decision::Allow, true),
            (&wanted[2], Decision::Deny, false),
        ]);
        let decided = validate(&request, &whole).unwrap();
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
