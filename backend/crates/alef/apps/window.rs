// SPDX-License-Identifier: MIT OR Apache-2.0
//! The runtime-owned application manager window and its local commands.
use std::{
    fs,
    path::{Path, PathBuf},
    sync::Arc,
};

use alef_core::{
    error::{AlefError, ErrorCode},
    registry::{command::Reply, dispatch::Registry, host::Host},
    security::{
        consent::{ConsentStore, Decision, Identity, Right},
        window::WindowDef,
    },
};
use alef_runtime::{Bridge, BridgeOptions, Commands, WindowOptions};
use serde::{Deserialize, Serialize};
use serde_json::json;
#[cfg(feature = "e2e")]
use serde_json::Value;

const PAGE: [(&str, &str); 3] = [
    ("index.html", include_str!("../assets/apps/index.html")),
    ("apps.css", include_str!("../assets/apps/apps.css")),
    ("apps.js", include_str!("../assets/apps/apps.js")),
];
#[cfg(feature = "e2e")]
const AUTOMATION: (&str, &str) = ("apps-e2e.js", include_str!("../assets/apps/apps-e2e.js"));

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct IdentityArgs {
    identity: Identity,
}
#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SetArgs {
    identity: Identity,
    right: Right,
    decision: Decision,
    #[serde(default)]
    confirm_red: bool,
}
#[derive(Debug, Serialize)]
struct IdentityResult {
    identity: Identity,
}

fn internal(error: impl ToString) -> AlefError {
    AlefError::new(ErrorCode::Internal, error.to_string())
}
fn invalid(error: impl ToString) -> AlefError {
    AlefError::new(ErrorCode::InvalidArgument, error.to_string())
}
fn store(home: &Path) -> alef_core::security::consent::FileConsentStore {
    alef_core::security::consent::FileConsentStore::new(home.join("consent"))
}
fn list(home: &Path, store: &dyn ConsentStore) -> Result<serde_json::Value, AlefError> {
    alef_launch::apps::AppsIndex::new(home, store)
        .list()
        .and_then(|rows| serde_json::to_value(rows).map_err(|error| error.to_string()))
        .map_err(internal)
}
fn describe(
    home: &Path,
    store: &dyn ConsentStore,
    id: &Identity,
) -> Result<serde_json::Value, AlefError> {
    alef_launch::apps::AppsIndex::new(home, store)
        .describe_identity(id)
        .and_then(|description| {
            serde_json::to_value(description).map_err(|error| error.to_string())
        })
        .map_err(internal)
}
fn set(
    home: &Path,
    store: &dyn ConsentStore,
    args: SetArgs,
) -> Result<serde_json::Value, AlefError> {
    let identity = alef_launch::apps::AppsIndex::new(home, store)
        .set_identity(&args.identity, &args.right, args.decision, args.confirm_red)
        .map_err(invalid)?;
    serde_json::to_value(IdentityResult { identity }).map_err(internal)
}
fn reset(
    home: &Path,
    store: &dyn ConsentStore,
    id: &Identity,
) -> Result<serde_json::Value, AlefError> {
    let identity = alef_launch::apps::AppsIndex::new(home, store)
        .reset_identity(id)
        .map_err(internal)?;
    serde_json::to_value(IdentityResult { identity }).map_err(internal)
}
fn install(registry: &mut Registry, host: Arc<dyn Host>, home: PathBuf) -> Result<(), AlefError> {
    install_commands(registry, host, home)?;
    #[cfg(feature = "e2e")]
    install_automation(registry)?;
    Ok(())
}
fn install_commands(
    registry: &mut Registry,
    host: Arc<dyn Host>,
    home: PathBuf,
) -> Result<(), AlefError> {
    registry
        .register_runtime::<()>("runtime.apps.list")?
        .handler({
            let home = home.clone();
            move |_, ()| {
                let home = home.clone();
                async move { Ok(Reply::Json(list(&home, &store(&home))?)) }
            }
        })?;
    registry
        .register_runtime::<IdentityArgs>("runtime.apps.describe")?
        .handler({
            let home = home.clone();
            move |_, args| {
                let home = home.clone();
                async move { Ok(Reply::Json(describe(&home, &store(&home), &args.identity)?)) }
            }
        })?;
    registry
        .register_runtime::<SetArgs>("runtime.apps.set")?
        .handler({
            let home = home.clone();
            move |_, args| {
                let home = home.clone();
                async move { Ok(Reply::Json(set(&home, &store(&home), args)?)) }
            }
        })?;
    registry
        .register_runtime::<IdentityArgs>("runtime.apps.reset")?
        .handler(move |_, args| {
            let home = home.clone();
            async move {
                let saved = reset(&home, &store(&home), &args.identity)?;
                Ok(Reply::Json(saved))
            }
        })?;
    registry
        .register_runtime::<()>("runtime.apps.close")?
        .handler(move |_, ()| {
            let host = host.clone();
            async move {
                host.quit(0);
                Ok(Reply::Json(serde_json::Value::Null))
            }
        })
}
#[cfg(feature = "e2e")]
fn install_automation(registry: &mut Registry) -> Result<(), AlefError> {
    if !std::env::var("ALEF_E2E").is_ok_and(|value| value == "1") {
        return Ok(());
    }
    registry
        .register_runtime::<()>("runtime.apps.automation")?
        .handler(|_, ()| async move {
            let raw = std::env::var("ALEF_E2E_APPS_UI").unwrap_or_default();
            let steps = if raw.is_empty() {
                Value::Null
            } else {
                serde_json::from_str(&raw).map_err(invalid)?
            };
            Ok(Reply::Json(steps))
        })
}

pub async fn run(icon: Vec<u8>) -> Result<(), String> {
    let pages = alef_launch::consent::ask::exchange_folder()
        .join(format!("{}-apps-page", std::process::id()));
    fs::create_dir_all(&pages).map_err(|error| format!("the page cannot be put down: {error}"))?;
    for (name, text) in PAGE {
        fs::write(pages.join(name), text)
            .map_err(|error| format!("the page cannot be put down: {error}"))?;
    }
    #[cfg(feature = "e2e")]
    if std::env::var("ALEF_E2E").is_ok_and(|value| value == "1") {
        fs::write(pages.join(AUTOMATION.0), AUTOMATION.1)
            .map_err(|error| format!("the page cannot be put down: {error}"))?;
        let index = PAGE[0].1.replace(
            "</head>",
            "<script type=\"module\" src=\"apps-e2e.js\"></script>\n</head>",
        );
        fs::write(pages.join("index.html"), index)
            .map_err(|error| format!("the page cannot be put down: {error}"))?;
    }
    let result = show(&pages, icon).await;
    let _ = fs::remove_dir_all(&pages);
    result
}
async fn show(pages: &Path, icon: Vec<u8>) -> Result<(), String> {
    let assets = alef_runtime::prepare_assets(
        alef_runtime::AssetInput::Disk {
            application_dir: pages
                .parent()
                .ok_or("Missing apps application boundary")?
                .to_owned(),
            selected_root: pages.to_owned(),
        },
        Some("/index.html"),
        &[],
    )
    .await
    .map_err(|error| error.to_string())?;
    let home = alef_launch::consent::runtime_home();
    let mut bridge = Bridge::with_options(
        Commands::new(),
        Some(assets),
        None,
        BridgeOptions {
            modules: Some(Box::new(move |registry, host| {
                install(registry, host, home)
            })),
            entry: Some("/index.html".to_owned()),
            ..BridgeOptions::default()
        },
    )
    .await
    .map_err(|error| error.to_string())?;
    let window: WindowDef = serde_json::from_value(json!({
        "label": "apps", "url": "/index.html", "width": 900, "height": 700,
        "minWidth": 560, "minHeight": 420, "title": "Applications",
    }))
    .map_err(|error| error.to_string())?;
    let result = alef_runtime::run(
        &mut bridge,
        WindowOptions {
            windows: vec![window],
            ..WindowOptions::new("Applications".to_owned(), icon)
        },
    );
    bridge.shutdown().await.map_err(|e| e.to_string())?;
    result.map_err(|e| e.to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;
    #[derive(Default)]
    struct MemoryStore(Mutex<Vec<(Identity, alef_core::security::consent::Consent)>>);
    impl ConsentStore for MemoryStore {
        fn load(
            &self,
            id: &Identity,
        ) -> std::io::Result<Option<alef_core::security::consent::Consent>> {
            let rows = self.0.lock().unwrap();
            if let Some((_, consent)) = rows.iter().find(|(identity, _)| identity == id) {
                return Ok(Some(consent.clone()));
            }
            Ok(None)
        }
        fn save(
            &self,
            id: &Identity,
            consent: &alef_core::security::consent::Consent,
        ) -> std::io::Result<()> {
            let mut rows = self.0.lock().unwrap();
            rows.retain(|(identity, _)| identity != id);
            rows.push((id.clone(), consent.clone()));
            Ok(())
        }
        fn forget(&self, id: &Identity) -> std::io::Result<()> {
            self.0
                .lock()
                .unwrap()
                .retain(|(identity, _)| identity != id);
            Ok(())
        }
        fn identities(&self) -> std::io::Result<Vec<Identity>> {
            let rows = self.0.lock().unwrap();
            Ok(rows.iter().map(|(identity, _)| identity.clone()).collect())
        }
    }
    fn fixture(root: &Path) -> PathBuf {
        let app = root.join("fixture-app");
        fs::create_dir_all(&app).unwrap();
        fs::write(app.join("alef.ktav"), "id: org.example.window-test\nname: Window test\nversion:: 1.0.0\nwindows: [\n    {\n        label: main\n        url: /index.html\n        width: 800\n        height: 600\n    }\n]\nexternal: {\n    connect: []\n    load: {\n        scripts: []\n        styles: []\n        images: []\n        fonts: []\n        media: []\n        frames: []\n    }\n}\npermissions: {\n    fs: {\n        read: []\n        write: []\n    }\n    cli: {\n        exec: [ sidecar:tool ]\n    }\n    net: {\n        http: []\n        socket: []\n    }\n    shell: {\n        openExternal: []\n    }\n    clipboard: {\n        read: true\n    }\n    shortcut: {\n        global: false\n    }\n    secrets: false\n    app: {\n        env: []\n    }\n}\n").unwrap();
        app
    }
    #[test]
    fn operations_enforce_index_policy_and_list_records() {
        let tmp = tempfile::tempdir().unwrap();
        let app = fixture(tmp.path());
        let home = tmp.path();
        let store = MemoryStore::default();
        let id = alef_launch::consent::identity_of(&app, "org.example.window-test").unwrap();
        let mut consent = alef_core::security::consent::Consent::undecided();
        let clipboard: Right = "clipboard.read".parse().unwrap();
        consent.set(clipboard.clone(), Decision::Allow);
        store.save(&id, &consent).unwrap();
        let rows = list(home, &store).unwrap();
        assert_eq!(rows.as_array().unwrap().len(), 1,);
        assert_eq!(rows[0]["summary"]["allow"], 1);
        let description = alef_launch::apps::AppsIndex::new(home, &store)
            .describe(&app)
            .unwrap();
        let red = description
            .rights
            .iter()
            .find(|right| right.red)
            .unwrap()
            .right
            .clone();
        let error = set(
            home,
            &store,
            SetArgs {
                identity: id.clone(),
                right: red.clone(),
                decision: Decision::Allow,
                confirm_red: false,
            },
        )
        .unwrap_err();
        assert!(error.message.contains("confirm-red"));
        assert_eq!(
            store.load(&id).unwrap().unwrap().decision(&red),
            Decision::Deny,
        );
        set(
            home,
            &store,
            SetArgs {
                identity: id.clone(),
                right: red.clone(),
                decision: Decision::Allow,
                confirm_red: true,
            },
        )
        .unwrap();
        assert_eq!(
            store.load(&id).unwrap().unwrap().decision(&red),
            Decision::Allow
        );
        let absent: Right = "notasked.right".parse().unwrap();
        assert!(set(
            home,
            &store,
            SetArgs {
                identity: id.clone(),
                right: absent,
                decision: Decision::Deny,
                confirm_red: true,
            }
        )
        .is_err());
        let other = Identity {
            app_id: "org.example.other".into(),
            signer: None,
        };
        let mut saved = alef_core::security::consent::Consent::undecided();
        saved.set(Right::plain("tray"), Decision::Deny);
        store.save(&other, &saved).unwrap();
        assert_eq!(list(home, &store).unwrap().as_array().unwrap().len(), 2,);
        set(
            home,
            &store,
            SetArgs {
                identity: id.clone(),
                right: clipboard.clone(),
                decision: Decision::Deny,
                confirm_red: false,
            },
        )
        .unwrap();
        let current = describe(home, &store, &id).unwrap();
        assert_eq!(
            current["rights"]
                .as_array()
                .unwrap()
                .iter()
                .find(|row| row["right"]["permission"] == "clipboard.read")
                .unwrap()["decision"],
            "deny"
        );
        reset(home, &store, &id).unwrap();
        let reset = describe(home, &store, &id).unwrap();
        assert!(reset["rights"]
            .as_array()
            .unwrap()
            .iter()
            .find(|row| row["right"]["permission"] == "clipboard.read")
            .unwrap()["decision"]
            .is_null());
    }
}
