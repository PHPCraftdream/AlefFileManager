// SPDX-License-Identifier: MIT OR Apache-2.0
//! The trust window: `alef trust <REQUEST> <ANSWER>`, which the launcher starts before the
//! application. The page is the runtime's own and nothing of the application runs in this
//! process; its commands (`runtime.trust.*`) exist only here.
use std::{fs, path::Path};

use alef_core::{
    error::{AlefError, ErrorCode},
    registry::{command::Reply, dispatch::Registry, host::Host},
    security::window::WindowDef,
};
use alef_runtime::{Bridge, BridgeOptions, Commands, WindowOptions};
use serde_json::{json, Value};

use crate::application::trust::TrustQuestion;
use crate::consent::ask::{self, exchange_folder, validate, write_answer, Answer, Request};

const PAGE: [(&str, &str); 3] = [
    ("index.html", include_str!("../../assets/trust/index.html")),
    ("trust.css", include_str!("../../assets/trust/trust.css")),
    ("trust.js", include_str!("../../assets/trust/trust.js")),
];

fn internal(error: impl ToString) -> AlefError {
    AlefError::new(ErrorCode::Internal, error.to_string())
}

/// The commands of the page: the question, the answer, and giving up.
fn install(
    registry: &mut Registry,
    host: std::sync::Arc<dyn Host>,
    request: std::sync::Arc<Request>,
    answer_path: std::path::PathBuf,
) -> Result<(), AlefError> {
    let asked = request.clone();
    registry
        .register_runtime::<()>("runtime.trust.request")?
        .handler(move |_, ()| {
            let asked = asked.clone();
            async move {
                Ok(Reply::Json(
                    serde_json::to_value(&*asked).map_err(internal)?,
                ))
            }
        })?;
    let (checked, leaving) = (request, host.clone());
    registry
        .register_runtime::<Answer>("runtime.trust.answer")?
        .handler(move |_, answer| {
            let (request, path, host) = (checked.clone(), answer_path.clone(), leaving.clone());
            async move {
                validate(&request, &answer)
                    .map_err(|message| AlefError::new(ErrorCode::InvalidArgument, message))?;
                write_answer(&path, &answer)?;
                host.quit(0);
                Ok(Reply::Json(Value::Null))
            }
        })?;
    registry
        .register_runtime::<()>("runtime.trust.cancel")?
        .handler(move |_, ()| {
            let host = host.clone();
            async move {
                host.quit(0);
                Ok(Reply::Json(Value::Null))
            }
        })
}

/// The trust question of one package, as the window asks it: no rights, only who signs the
/// application.
pub fn request_for_trust(app: &ask::AppSummary, question: &TrustQuestion) -> Request {
    Request {
        app: app.clone(),
        rights: Vec::new(),
        trust: Some(match question {
            TrustQuestion::Signed { fingerprint } => ask::TrustAsk {
                fingerprint: Some(fingerprint.clone()),
                archive_digest: None,
            },
            TrustQuestion::Unsigned { archive_digest } => ask::TrustAsk {
                fingerprint: None,
                archive_digest: Some(archive_digest.clone()),
            },
        }),
        #[cfg(feature = "e2e")]
        automation: None,
    }
}

/// Shows the window in a process of its own and waits for it. `Ok(None)`: the user closed it
/// without answering. The files of the exchange are removed whatever happens.
pub fn ask_in_window(request: &Request) -> Result<Option<bool>, String> {
    let folder = exchange_folder();
    fs::create_dir_all(&folder)
        .map_err(|e| format!("the trust question cannot be put down: {e}"))?;
    let stem = format!("{}", std::process::id());
    let request_path = folder.join(format!("{stem}.trust.request.json"));
    let answer_path = folder.join(format!("{stem}.trust.answer.json"));
    let _ = fs::remove_file(&answer_path);
    let outcome = (|| {
        fs::write(
            &request_path,
            serde_json::to_vec(request).map_err(|e| e.to_string())?,
        )
        .map_err(|e| format!("the trust question cannot be put down: {e}"))?;
        let exe =
            std::env::current_exe().map_err(|e| format!("the runtime cannot be found: {e}"))?;
        let status = std::process::Command::new(exe)
            .arg("trust")
            .arg(&request_path)
            .arg(&answer_path)
            .status()
            .map_err(|e| format!("the trust window cannot be started: {e}"))?;
        let bytes = match fs::read(&answer_path) {
            Ok(bytes) => bytes,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
                return if status.success() {
                    Ok(None)
                } else {
                    Err(format!("the trust window ended with {status}"))
                };
            }
            Err(error) => return Err(format!("the answer cannot be read: {error}")),
        };
        let answer: Answer =
            serde_json::from_slice(&bytes).map_err(|e| format!("the answer is damaged: {e}"))?;
        validate(request, &answer).map(|(_, trust)| trust)
    })();
    let _ = fs::remove_file(&request_path);
    let _ = fs::remove_file(&answer_path);
    outcome
}

/// Shows the trust window until the user answers or closes it; the answer is written to
/// `answer_path`, and nothing is written when the window is closed.
pub async fn run(request_path: &Path, answer_path: &Path, icon: Vec<u8>) -> Result<(), String> {
    let request: Request = serde_json::from_slice(
        &fs::read(request_path).map_err(|e| format!("the question cannot be read: {e}"))?,
    )
    .map_err(|e| format!("the question is damaged: {e}"))?;
    let pages = exchange_folder().join(format!("{}-trust-page", std::process::id()));
    fs::create_dir_all(&pages).map_err(|e| format!("the page cannot be put down: {e}"))?;
    for (name, text) in PAGE {
        fs::write(pages.join(name), text)
            .map_err(|e| format!("the page cannot be put down: {e}"))?;
    }
    let shown = show(request, answer_path, &pages, icon).await;
    let _ = fs::remove_dir_all(&pages);
    shown
}

async fn show(
    request: Request,
    answer_path: &Path,
    pages: &Path,
    icon: Vec<u8>,
) -> Result<(), String> {
    let request = std::sync::Arc::new(request);
    let answer_path = answer_path.to_owned();
    let mut bridge = Bridge::with_options(
        Commands::new(),
        Some(alef_runtime::AssetInput::Disk {
            application_dir: pages
                .parent()
                .ok_or("Missing trust application boundary")?
                .to_owned(),
            selected_root: pages.to_owned(),
        }),
        None,
        BridgeOptions {
            modules: Some(Box::new(move |registry, host| {
                install(registry, host, request, answer_path)
            })),
            entry: Some("/index.html".to_owned()),
            ..BridgeOptions::default()
        },
    )
    .await
    .map_err(|e| e.to_string())?;
    let window: WindowDef = serde_json::from_value(json!({
        "label": "trust",
        "url": "/index.html",
        "width": 640,
        "height": 520,
        "minWidth": 460,
        "minHeight": 380,
        "title": "Trust",
    }))
    .map_err(|e| e.to_string())?;
    let result = alef_runtime::run(
        &mut bridge,
        WindowOptions {
            windows: vec![window],
            ..WindowOptions::new("Trust".to_owned(), icon)
        },
    );
    bridge.shutdown().await.map_err(|e| e.to_string())?;
    result.map_err(|e| e.to_string())
}
