// SPDX-License-Identifier: MIT OR Apache-2.0
//! A registry with every module registered, a fake host and a session, to call commands by name.
use std::{
    ffi::OsString,
    sync::{Arc, Mutex},
};

use alef_core::{
    protocol::call::Limits,
    registry::{
        command::Reply,
        context::CallContext,
        dispatch::Registry,
        host::{Host, Theme},
    },
    security::{
        manifest::Manifest,
        permissions::{PathVars, PermissionSet},
    },
    session::{session::SessionManager, Session, TokenSource},
    AlefError,
};
use alef_modules::{desktop::args, register_all, AppInfo, ModuleContext};
use serde_json::Value;

const MANIFEST: &str = include_str!("../fixtures/app.ktav");

pub struct FakeHost {
    pub quits: Mutex<Vec<i32>>,
    pub theme: Mutex<Theme>,
}

impl Host for FakeHost {
    fn quit(&self, code: i32) {
        self.quits.lock().unwrap().push(code);
    }
    fn theme(&self) -> Theme {
        *self.theme.lock().unwrap()
    }
}

pub struct Fixture {
    pub registry: Registry,
    pub host: Arc<FakeHost>,
    pub context: ModuleContext,
    session: Arc<Session>,
    permissions: Arc<PermissionSet>,
}

pub fn path_vars(root: &std::path::Path) -> PathVars {
    let at = |name: &str| root.join(name);
    PathVars {
        app_data: at("data"),
        app_config: at("config"),
        app_cache: at("cache"),
        home: at("home"),
        documents: at("documents"),
        downloads: at("downloads"),
        desktop: at("desktop"),
        temp: at("temp"),
        app: at("app"),
    }
}

impl Fixture {
    /// `manifest` replaces the fixture manifest when given; `command_line` is parsed by its schema.
    pub async fn new(manifest: Option<&str>, command_line: &[&str]) -> Self {
        let text = manifest.unwrap_or(MANIFEST).replace('\r', "");
        let manifest = Manifest::from_ktav_str(&text).expect("manifest");
        let root = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .join("alef-modules-paths");
        let vars = path_vars(&root);
        let permissions = Arc::new(
            PermissionSet::from_manifest(&manifest.permissions, &vars).expect("permissions"),
        );
        let raw: Vec<OsString> = command_line.iter().map(OsString::from).collect();
        let args = match args::parse(
            manifest.arguments.as_ref(),
            &manifest.name,
            &manifest.version,
            &raw,
        )
        .expect("command line")
        {
            args::Parsed::Run(parsed) => parsed,
            other => panic!("expected a run, got {other:?}"),
        };
        let context = ModuleContext {
            app: AppInfo {
                id: manifest.id.clone(),
                name: manifest.name.clone(),
                version: manifest.version.clone(),
                runtime_version: "9.9.9".to_owned(),
            },
            paths: vars,
            args,
            process_args: raw,
        };
        let host = Arc::new(FakeHost {
            quits: Mutex::new(Vec::new()),
            theme: Mutex::new(Theme::Light),
        });
        let mut registry = Registry::default();
        register_all(&mut registry, host.clone(), &context).expect("register");
        let tokens: TokenSource = Arc::new(|| "token".to_owned());
        let session = SessionManager::new(tokens, Limits::default())
            .begin_document(1)
            .await;
        Self {
            registry,
            host,
            context,
            session,
            permissions,
        }
    }

    pub async fn call(&self, command: &str, args: Value) -> Result<Value, AlefError> {
        let ctx = CallContext::new(self.session.clone(), self.permissions.clone());
        match self.registry.dispatch(command, ctx, args).await? {
            Reply::Json(value) => Ok(value),
            other => panic!("expected JSON, got {other:?}"),
        }
    }
}
