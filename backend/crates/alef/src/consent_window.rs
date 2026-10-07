// SPDX-License-Identifier: MIT OR Apache-2.0
//! The permission window: `alef consent <REQUEST> <ANSWER>`, which `alef --app` starts before the
//! application. The page is the runtime's own and nothing of the application runs in this process;
//! its commands (`runtime.consent.*`) exist only here, so an application, whose process has a
//! registry without them, cannot call them.
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use alef_core::{
    error::{AlefError, ErrorCode},
    registry::{command::Reply, dispatch::Registry, host::Host},
    security::window::WindowDef,
};
use alef_launch::consent::ask::{exchange_folder, validate, write_answer, Answer, Request};
use alef_runtime::{Bridge, BridgeOptions, Commands, WindowOptions};
use serde_json::{json, Value};

const PAGE: [(&str, &str); 3] = [
    ("index.html", include_str!("../assets/consent/index.html")),
    ("consent.css", include_str!("../assets/consent/consent.css")),
    ("consent.js", include_str!("../assets/consent/consent.js")),
];

/// The clicks of an end-to-end run; written only when the run asks for them.
const AUTOMATION: (&str, &str) = (
    "consent-e2e.js",
    include_str!("../assets/consent/consent-e2e.js"),
);

fn internal(error: impl ToString) -> AlefError {
    AlefError::new(ErrorCode::Internal, error.to_string())
}

/// The commands of the page: the question, the answer, and giving up.
fn install(
    registry: &mut Registry,
    host: Arc<dyn Host>,
    request: Arc<Request>,
    answer_path: PathBuf,
) -> Result<(), AlefError> {
    let asked = request.clone();
    registry
        .register_runtime::<()>("runtime.consent.request")?
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
        .register_runtime::<Answer>("runtime.consent.answer")?
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
        .register_runtime::<()>("runtime.consent.cancel")?
        .handler(move |_, ()| {
            let host = host.clone();
            async move {
                host.quit(0);
                Ok(Reply::Json(Value::Null))
            }
        })
}

/// Shows the window until the user answers or closes it; the answer is written to `answer_path`,
/// and nothing is written when the window is closed.
pub async fn run(request_path: &Path, answer_path: &Path, icon: Vec<u8>) -> Result<(), String> {
    let request: Request = serde_json::from_slice(
        &fs::read(request_path).map_err(|e| format!("the question cannot be read: {e}"))?,
    )
    .map_err(|e| format!("the question is damaged: {e}"))?;
    let pages = exchange_folder().join(format!("{}-page", std::process::id()));
    fs::create_dir_all(&pages).map_err(|e| format!("the page cannot be put down: {e}"))?;
    let mut files = PAGE.to_vec();
    if request.automation.is_some() && std::env::var("ALEF_E2E").is_ok_and(|v| v == "1") {
        files.push(AUTOMATION);
    }
    for (name, text) in files {
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
    let request = Arc::new(request);
    let answer_path = answer_path.to_owned();
    let mut bridge = Bridge::with_options(
        Commands::new(),
        Some(pages),
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
        "label": "consent",
        "url": "/index.html",
        "width": 680,
        "height": 780,
        "minWidth": 460,
        "minHeight": 420,
        "title": "Permissions",
    }))
    .map_err(|e| e.to_string())?;
    let result = alef_runtime::run(
        &mut bridge,
        WindowOptions {
            windows: vec![window],
            ..WindowOptions::new("Permissions".to_owned(), icon)
        },
    );
    bridge.shutdown().await.map_err(|e| e.to_string())?;
    result.map_err(|e| e.to_string())
}
