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
    desktop::args, register_all, AppInfo, Backends, Console, MemoryAutostart, MemoryClipboard,
    MemoryDeepLinks, MemorySecrets, ModuleContext, PretendNotifications, PretendShell, Termination,
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
    pub deep_links: Arc<MemoryDeepLinks>,
    pub autostart: Arc<MemoryAutostart>,
    pub registry: Registry,
    pub host: Arc<FakeHost>,
    /// The clipboard and the shell behind the modules: in memory, and doing nothing.
    pub clipboard: Arc<MemoryClipboard>,
    pub shell: Arc<PretendShell>,
    pub notifications: Arc<PretendNotifications>,
    /// The secrets behind the module: in memory.
    pub secrets: Arc<MemorySecrets>,
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
        let root = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .join("alef-modules-paths");
        Self::new_in(&root, manifest, command_line).await
    }

    /// As `new`, with the folders of the application (data, config, ...) under `root`: a test that
    /// keeps data gets its own, and a second fixture on the same root is the next run of the application.
    pub async fn new_in(
        root: &std::path::Path,
        manifest: Option<&str>,
        command_line: &[&str],
    ) -> Self {
        Self::new_sharing(
            root,
            manifest,
            command_line,
            Arc::new(MemorySecrets::default()),
        )
        .await
    }

    /// As `new_in`, with the store of secrets that another fixture has: two applications on one machine.
    pub async fn new_sharing(
        root: &std::path::Path,
        manifest: Option<&str>,
        command_line: &[&str],
        secrets: Arc<MemorySecrets>,
    ) -> Self {
        Self::build(
            root,
            manifest,
            command_line,
            secrets,
            None,
            Limits::default(),
        )
        .await
    }

    /// As `new`, with the limits of the sessions given: a small stream window shows a flow control that the default one hides.
    pub async fn new_limited(manifest: Option<&str>, limits: Limits) -> Self {
        let root = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .join("alef-modules-paths");
        Self::build(
            &root,
            manifest,
            &[],
            Arc::new(MemorySecrets::default()),
            None,
            limits,
        )
        .await
    }

    /// A console utility: the standard streams of the process are `console`'s.
    pub async fn new_console(console: Console, manifest: Option<&str>) -> Self {
        let root = std::env::current_exe()
            .unwrap()
            .parent()
            .unwrap()
            .join("alef-modules-paths");
        Self::build(
            &root,
            manifest,
            &[],
            Arc::new(MemorySecrets::default()),
            Some(console),
            Limits::default(),
        )
        .await
    }

    async fn build(
        root: &std::path::Path,
        manifest: Option<&str>,
        command_line: &[&str],
        secrets: Arc<MemorySecrets>,
        console: Option<Console>,
        limits: Limits,
    ) -> Self {
        let text = manifest.unwrap_or(MANIFEST).replace('\r', "");
        let manifest = Manifest::from_ktav_str(&text).expect("manifest");
        let root = root.to_path_buf();
        let vars = path_vars(&root);
        let permissions = Arc::new(
            PermissionSet::from_manifest(&manifest.permissions, &vars)
                .expect("permissions")
                .with_deep_links(&manifest.deep_links)
                .expect("deep links"),
        );
        let raw: Vec<OsString> = command_line.iter().map(OsString::from).collect();
        let deep_links = Arc::new(MemoryDeepLinks::default());
        let autostart = Arc::new(MemoryAutostart::default());
        let clipboard = Arc::new(MemoryClipboard::default());
        let shell = Arc::new(PretendShell::default());
        let notifications = Arc::new(PretendNotifications::default());
        let (application_args, startup_urls) =
            args::extract_deep_links(&manifest.deep_links, &raw).expect("deep-link arguments");
        let args = match args::parse(
            manifest.arguments.as_ref(),
            &manifest.name,
            &manifest.version,
            &application_args,
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
            deep_link_schemes: manifest.deep_links.clone(),
            startup_urls,
            process_args: raw,
            backends: Backends {
                deep_links: deep_links.clone(),
                autostart: autostart.clone(),
                clipboard: clipboard.clone(),
                shell: shell.clone(),
                notification: notifications.clone(),
                secrets: secrets.clone(),
            },
            // Its own for every fixture: tests run side by side.
            shadow: root.join(format!(
                "shadow-{}",
                SHADOWS.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
            )),
            console,
            termination: Termination::default(),
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
        let manager = SessionManager::new(tokens, limits);
        let session = manager.begin_document(1).await;
        Self {
            deep_links,
            autostart,
            registry,
            host,
            clipboard,
            shell,
            notifications,
            secrets,
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

#[cfg(test)]
mod deep_link_registration_tests {
    use super::*;
    use alef_core::{
        security::consent::{Consent, Decision, Right},
        ErrorCode,
    };
    use serde_json::json;

    fn manifest() -> String {
        format!("{MANIFEST}\ndeepLinks: [\n    :: sample\n    :: other\n]\n")
    }
    fn consent(first: Decision, second: Decision) -> Consent {
        let mut consent = Consent::undecided();
        consent.set(Right::scoped("app.deepLinks", "sample"), first);
        consent.set(Right::scoped("app.deepLinks", "other"), second);
        consent
    }
    #[tokio::test]
    async fn deep_link_registration_checks_all_scopes_before_any_backend_call() {
        for (first, second) in [
            (Decision::Allow, Decision::Deny),
            (Decision::Substitute, Decision::Deny),
            (Decision::Deny, Decision::Allow),
        ] {
            let fixture = Fixture::new(Some(&manifest()), &[])
                .await
                .with_consent(consent(first, second));
            for command in ["app.registerDeepLinks", "app.unregisterDeepLinks"] {
                assert_eq!(
                    fixture.call(command, Value::Null).await.unwrap_err().code,
                    ErrorCode::PermissionDenied
                );
            }
            assert!(fixture.deep_links.calls().is_empty());
        }
    }
    #[tokio::test]
    async fn deep_link_registration_diverts_substituted_scopes_and_rejects_arguments() {
        for (first, second, expected) in [
            (Decision::Allow, Decision::Allow, vec!["sample", "other"]),
            (Decision::Allow, Decision::Substitute, vec!["sample"]),
            (Decision::Substitute, Decision::Allow, vec!["other"]),
            (Decision::Substitute, Decision::Substitute, vec![]),
        ] {
            let fixture = Fixture::new(Some(&manifest()), &[])
                .await
                .with_consent(consent(first, second));
            for args in [
                json!({"schemes": ["injected"]}),
                json!(["sample"]),
                json!(true),
            ] {
                assert_eq!(
                    fixture
                        .call("app.registerDeepLinks", args)
                        .await
                        .unwrap_err()
                        .code,
                    ErrorCode::InvalidArgument
                );
            }
            assert!(fixture.deep_links.calls().is_empty());
            fixture
                .call("app.registerDeepLinks", Value::Null)
                .await
                .unwrap();
            fixture
                .call("app.unregisterDeepLinks", json!({}))
                .await
                .unwrap();
            let calls = fixture.deep_links.calls();
            if expected.is_empty() {
                assert!(calls.is_empty());
            } else {
                assert_eq!(calls.len(), 2);
                assert_eq!(calls[0].2, expected);
                assert!(calls[0].3);
                assert!(!calls[1].3);
            }
        }
    }
    #[tokio::test]
    async fn deep_link_registration_empty_and_narrowed_declarations_are_denied() {
        let fixture = Fixture::new(None, &[]).await;
        assert_eq!(
            fixture
                .call("app.registerDeepLinks", Value::Null)
                .await
                .unwrap_err()
                .code,
            ErrorCode::PermissionDenied
        );
        let mut fixture = Fixture::new(Some(&manifest()), &[]).await;
        fixture.permissions = Arc::new(
            PermissionSet::from_manifest(
                &Manifest::from_ktav_str(MANIFEST).unwrap().permissions,
                &fixture.context.paths,
            )
            .unwrap()
            .with_deep_links(&["sample".into()])
            .unwrap()
            .with_consent(consent(Decision::Allow, Decision::Allow)),
        );
        assert_eq!(
            fixture
                .call("app.registerDeepLinks", Value::Null)
                .await
                .unwrap_err()
                .code,
            ErrorCode::PermissionDenied
        );
        assert!(fixture.deep_links.calls().is_empty());
    }
    #[test]
    fn memory_deep_links_keeps_only_256_entries() {
        use alef_modules::DeepLinkBackend;
        let memory = MemoryDeepLinks::default();
        let folder = std::env::current_dir().unwrap();
        for _ in 0..300 {
            memory
                .apply("example", &folder, &["sample".into()], true)
                .unwrap();
        }
        assert_eq!(memory.calls().len(), 256);
        assert!(memory
            .apply("example", &folder, &["https".into()], true)
            .is_err());
    }
}
