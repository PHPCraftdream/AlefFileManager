// SPDX-License-Identifier: MIT OR Apache-2.0
//! A registry with every module registered, a fake host and a session, to call commands by name.
#![allow(dead_code)] // each test binary uses a different part
use std::{
    collections::VecDeque,
    ffi::OsString,
    sync::{Arc, Mutex},
};

use alef_core::{
    protocol::call::Limits,
    registry::{
        command::Reply,
        context::CallContext,
        dispatch::Registry,
        host::{Host, HostFuture, Theme},
        window::UiCall,
    },
    security::{
        manifest::Manifest,
        permissions::{PathVars, PermissionSet},
    },
    session::{session::SessionManager, Session, TokenSource},
    AlefError,
};
use alef_modules::{
    desktop::args, register_all, AppInfo, Backends, MemoryClipboard, ModuleContext,
    PretendNotifications, PretendShell,
};
use bytes::Bytes;
use serde_json::Value;

const MANIFEST: &str = include_str!("../fixtures/app.ktav");
static SHADOWS: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);

/// The tests that change the desktop of the user (clipboard, trash) run only when asked.
pub fn desktop_asked() -> bool {
    std::env::var("ALEF_TEST_DESKTOP").is_ok_and(|value| value == "1")
}

pub struct FakeHost {
    pub quits: Mutex<Vec<i32>>,
    pub theme: Mutex<Theme>,
    /// What `Host::ui` was asked: the calling window and the call.
    pub calls: Mutex<Vec<(u64, UiCall)>>,
    /// Answers of `Host::ui`, first in first out; `null` when none is queued.
    pub replies: Mutex<VecDeque<Result<Value, AlefError>>>,
    /// The events the modules sent: the window they went to (None: every window), name, payload.
    pub events: Mutex<Vec<(Option<u64>, String, Value)>>,
}

impl Host for FakeHost {
    fn quit(&self, code: i32) {
        self.quits.lock().unwrap().push(code);
    }
    fn theme(&self) -> Theme {
        *self.theme.lock().unwrap()
    }
    fn emit(&self, window: Option<u64>, name: &str, payload: Value) {
        self.events
            .lock()
            .unwrap()
            .push((window, name.to_owned(), payload));
    }
    fn ui(&self, caller: u64, call: UiCall) -> HostFuture {
        self.calls.lock().unwrap().push((caller, call));
        let reply = self
            .replies
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Ok(Value::Null));
        Box::pin(async move { reply })
    }
}

pub struct Fixture {
    pub registry: Registry,
    pub host: Arc<FakeHost>,
    /// The clipboard and the shell behind the modules: in memory, and doing nothing.
    pub clipboard: Arc<MemoryClipboard>,
    pub shell: Arc<PretendShell>,
    pub notifications: Arc<PretendNotifications>,
    pub context: ModuleContext,
    manager: SessionManager,
    session: Arc<Session>,
    permissions: Arc<PermissionSet>,
}

pub fn path_vars(root: &std::path::Path) -> PathVars {
    let at = |name: &str| root.join(name);
    PathVars {
        app_data: at("data"),
        app_config: at("config"),
        // Short: a Unix socket path of the single instance lives here, and it must not be long.
        app_cache: std::env::temp_dir().join("alef-modules-cache"),
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
        let clipboard = Arc::new(MemoryClipboard::default());
        let shell = Arc::new(PretendShell::default());
        let notifications = Arc::new(PretendNotifications::default());
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
            backends: Backends {
                clipboard: clipboard.clone(),
                shell: shell.clone(),
                notification: notifications.clone(),
            },
            // Its own for every fixture: tests run side by side.
            shadow: root.join(format!(
                "shadow-{}",
                SHADOWS.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            )),
        };
        let host = Arc::new(FakeHost {
            quits: Mutex::new(Vec::new()),
            theme: Mutex::new(Theme::Light),
            calls: Mutex::new(Vec::new()),
            replies: Mutex::new(VecDeque::new()),
            events: Mutex::new(Vec::new()),
        });
        let mut registry = Registry::default();
        register_all(&mut registry, host.clone(), &context).expect("register");
        let tokens: TokenSource = Arc::new(|| "token".to_owned());
        let manager = SessionManager::new(tokens, Limits::default());
        let session = manager.begin_document(1).await;
        Self {
            registry,
            host,
            clipboard,
            shell,
            notifications,
            context,
            manager,
            session,
            permissions,
        }
    }

    /// Another window loads its document: its session, to call commands as that document.
    pub async fn open_window(&self, window: u64) -> Arc<Session> {
        self.manager.begin_document(window).await
    }

    /// Calls a command as the document of `session`.
    pub async fn call_as(
        &self,
        session: &Arc<Session>,
        command: &str,
        args: Value,
    ) -> Result<Value, AlefError> {
        let ctx = CallContext::new(session.clone(), self.permissions.clone());
        match self.registry.dispatch(command, ctx, args).await? {
            Reply::Json(value) => Ok(value),
            other => panic!("expected JSON, got {other:?}"),
        }
    }

    /// The user decided: the rights of the manifest are given as `consent` says.
    pub fn with_consent(mut self, consent: alef_core::security::consent::Consent) -> Self {
        self.permissions = Arc::new((*self.permissions).clone().with_consent(consent));
        self
    }

    pub fn session(&self) -> Arc<Session> {
        self.session.clone()
    }

    pub fn permissions(&self) -> Arc<PermissionSet> {
        self.permissions.clone()
    }

    /// The document of the window loads again: a new session replaces the old one, grants and all.
    pub async fn reload_document(&mut self) {
        self.session = self.manager.begin_document(1).await;
    }

    pub async fn call_reply(
        &self,
        command: &str,
        args: Value,
        body: Option<Bytes>,
    ) -> Result<Reply, AlefError> {
        let ctx = CallContext::new(self.session.clone(), self.permissions.clone()).with_body(body);
        self.registry.dispatch(command, ctx, args).await
    }

    pub async fn call(&self, command: &str, args: Value) -> Result<Value, AlefError> {
        match self.call_reply(command, args, None).await? {
            Reply::Json(value) => Ok(value),
            other => panic!("expected JSON, got {other:?}"),
        }
    }
}
