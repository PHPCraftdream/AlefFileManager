// SPDX-License-Identifier: MIT OR Apache-2.0
//! The `native://` protocol: application assets and the transport routes `call`/`stream`.
pub(crate) mod assets;
pub(crate) mod commands;
#[cfg(feature = "e2e")]
mod e2e;
mod events;
mod transport;
mod windows;

use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::sync::Arc;

use alef_core::{
    error::AlefError,
    protocol::{
        call::Limits,
        transport::{Transport, TransportConfig},
    },
    registry::{dispatch::Registry, host::Host},
    security::{
        manifest::{
            AppPermissions, CliPermissions, ClipboardPermissions, FsPermissions, NetPermissions,
            Permissions, ShellPermissions, ShortcutPermissions,
        },
        permissions::{PathVars, PermissionSet},
    },
    session::{session::SessionManager, TokenSource},
};
use http::{header, HeaderValue};
use serde_json::json;
use servo::protocol_handler::{
    DoneChannel, FetchContext, HttpStatus, ProtocolHandler, ProtocolRegistry, Request,
    ResourceFetchTiming, Response, ResponseBody,
};
use tokio::sync::mpsc;
use url::Url;

use crate::ui::UiRequest;
use crate::{Commands, RuntimeHandle};
pub(crate) use events::EventBus;
pub(crate) use windows::WindowRegistry;

/// Content-Security-Policy of `native://app` documents (embedder/consent paths) unless the
/// embedder supplies its own; https-origin documents use `assets::origin::DEFAULT_ORIGIN_CSP`.
const DEFAULT_CSP: &str = "default-src native:; connect-src native:; img-src native: data:; style-src native: 'unsafe-inline'; object-src 'none'; frame-src 'none'; base-uri 'none'";

/// Registers the command modules (`alef-modules`) in the registry of the bridge; it receives the
/// hosting process (exit request, theme) for the modules to keep.
pub type ModuleInstaller =
    Box<dyn FnOnce(&mut Registry, Arc<dyn Host>) -> Result<(), AlefError> + Send>;

/// Owned asset capability; archive packages must own their immutable snapshot bytes.
pub enum AssetInput {
    Disk {
        application_dir: PathBuf,
        selected_root: PathBuf,
    },
    Archive {
        package: alef_package::Package<std::io::Cursor<Vec<u8>>>,
        selected_root: String,
    },
}

/// A built and audited asset source whose entry and initial window documents were already
/// checked through the same lookup that serves requests; build it with [`prepare_assets`]
/// before any trust or consent decision and hand it to [`Bridge::with_options`].
pub struct PreparedAssets {
    pub(crate) source: Arc<assets::Source>,
    pub(crate) selected_root: Option<PathBuf>,
    pub(crate) archive: bool,
}

/// Builds the source once and checks the entry document and every initial window document
/// through the request lookup; every failure is the same `io::Error` the bridge returns.
pub async fn prepare_assets(
    input: AssetInput,
    entry: Option<&str>,
    initial_document_urls: &[String],
) -> io::Result<PreparedAssets> {
    let native_entry = native_entry(entry)?;
    let selected_root = match &input {
        AssetInput::Disk { selected_root, .. } => Some(selected_root.clone()),
        AssetInput::Archive { .. } => None,
    };
    let archive = matches!(input, AssetInput::Archive { .. });
    let source = Arc::new(assets::Source::prepare(input)?);
    check_documents(&source, native_entry, initial_document_urls).await?;
    Ok(PreparedAssets {
        source,
        selected_root,
        archive,
    })
}

fn native_entry(entry: Option<&str>) -> io::Result<Url> {
    let entry = entry_path(entry)?;
    Url::parse("native://app/")
        .and_then(|base| base.join(&entry))
        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))
}

async fn check_documents(
    source: &Arc<assets::Source>,
    native_entry: Url,
    initial_document_urls: &[String],
) -> io::Result<()> {
    read_named(source, native_entry.path()).await?;
    for reference in initial_document_urls {
        let url = native_entry
            .join(reference)
            .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
        if (url.scheme(), url.host_str(), url.port()) != ("native", Some("app"), None) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "Initial document must belong to the application",
            ));
        }
        read_named(source, url.path()).await?;
    }
    Ok(())
}

/// Reads one document through the request lookup; a failure names the document and keeps its kind.
async fn read_named(source: &Arc<assets::Source>, path: &str) -> io::Result<()> {
    assets::read_asset(Some(source.clone()), path.to_owned())
        .await
        .map(drop)
        .map_err(|error| io::Error::new(error.kind(), format!("{path}: {error}")))
}

/// Embedder-supplied policy of the bridge; the default suits the File Manager.
#[derive(Default)]
pub struct BridgeOptions {
    /// Installs the framework modules; `None` serves only the commands of the embedder.
    pub modules: Option<ModuleInstaller>,
    /// Allowed `Origin` values for transport requests; empty means any (the bootstrap token is
    /// then the only secret).
    pub allowed_origins: Vec<String>,
    /// CSP of `native://app` documents; `None` keeps the built-in default.
    pub csp: Option<String>,
    /// Permission policy of commands; `None` is closed (only commands without a permission run).
    pub permissions: Option<Arc<PermissionSet>>,
    /// Per-session transport limits.
    pub limits: Limits,
    /// Root-relative entry document (`/app.html`); `None` opens the root, served as `index.html`.
    pub entry: Option<String>,
    /// Initial document references, resolved against the entry URL and preflighted before modules.
    pub initial_document_urls: Vec<String>,
    /// DNS label of the application origin (from `alef_core::security::origin`); `Some` loads the
    /// entry from `https://<label>.alef` and installs the https interception. `None` keeps
    /// `native://app`.
    pub origin_host: Option<String>,
    /// Origins the manifest's `external` section allows; the interception refuses every other
    /// external http(s) request before the network.
    pub allowed_external_origins: Vec<String>,
    /// Servo profile root (`Opts.config_dir`): web storage and the disk cache live under it.
    /// `None` keeps the Servo default.
    pub profile_dir: Option<std::path::PathBuf>,
    /// Called when the headless entry finishes loading; runtime owns no module state.
    pub headless_loaded: Option<Box<dyn Fn() + Send>>,
    /// Called each headless iteration; true requests a launch-timeout exit (code 2).
    pub headless_tick: Option<Box<dyn Fn() -> bool + Send>>,
}

pub struct Bridge {
    pub entry_url: Url,
    registry: Option<ProtocolRegistry>,
    commands: Arc<Commands>,
    ui: RuntimeHandle,
    requests: Option<mpsc::Receiver<UiRequest>>,
    sessions: Arc<SessionManager>,
    windows: WindowRegistry,
    transport: Arc<Transport>,
    pub(crate) headless_loaded: Option<Box<dyn Fn() + Send>>,
    pub(crate) headless_tick: Option<Box<dyn Fn() -> bool + Send>>,
    pub(crate) profile_dir: Option<PathBuf>,
    /// Every document of this bridge gets the WebCrypto stand-in (the right was not granted).
    pub(crate) webcrypto_substitute: bool,
    /// RAII owner of the installed interception site; cleared on drop, never inherited.
    site: Option<assets::origin::SiteGuard>,
    /// Test pin that origin mode disables `native://app` assets.
    #[cfg(test)]
    native_assets: Option<Arc<assets::Source>>,
}

/// A root-relative path (`/` or `/dir/app.html?x=1`); anything that could name another host or
/// scheme is refused.
fn entry_path(entry: Option<&str>) -> io::Result<String> {
    let entry = entry.unwrap_or("/");
    let valid = entry.starts_with('/')
        && !entry.starts_with("//")
        && !entry.contains('\\')
        && Url::parse("native://app/")
            .and_then(|base| base.join(entry))
            .is_ok_and(|url| url.host_str() == Some("app") && url.scheme() == "native");
    if valid {
        Ok(entry.to_owned())
    } else {
        Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            "The entry document must be a root-relative path",
        ))
    }
}

fn random_token() -> io::Result<String> {
    let mut random = [0u8; 32];
    getrandom::fill(&mut random).map_err(io::Error::other)?;
    const HEX: &[u8; 16] = b"0123456789abcdef";
    let mut token = String::with_capacity(64);
    for byte in random {
        token.push(char::from(HEX[usize::from(byte >> 4)]));
        token.push(char::from(HEX[usize::from(byte & 15)]));
    }
    Ok(token)
}

fn startup(error: AlefError) -> io::Error {
    io::Error::other(format!("{}: {}", error.code.as_str(), error.message))
}

/// A policy that allows nothing: commands without a permission still run.
fn closed_permissions() -> io::Result<Arc<PermissionSet>> {
    let policy = Permissions {
        fs: FsPermissions {
            read: Vec::new(),
            write: Vec::new(),
        },
        cli: CliPermissions {
            exec: Vec::new(),
            commands: Vec::new(),
        },
        net: NetPermissions {
            http: Vec::new(),
            socket: Vec::new(),
        },
        shell: ShellPermissions {
            open_external: Vec::new(),
        },
        clipboard: ClipboardPermissions { read: false },
        shortcut: ShortcutPermissions { global: false },
        tray: false,
        secrets: false,
        webcrypto: false,
        app: AppPermissions {
            env: Vec::new(),
            autostart: false,
        },
        window: None,
    };
    let root = std::env::temp_dir();
    let vars = PathVars {
        app_data: root.clone(),
        app_config: root.clone(),
        app_cache: root.clone(),
        home: root.clone(),
        documents: root.clone(),
        downloads: root.clone(),
        desktop: root.clone(),
        temp: root.clone(),
        app: Some(root),
    };
    PermissionSet::from_manifest(&policy, &vars)
        .map(Arc::new)
        .map_err(startup)
}

/// Build the same command table for startup and registry-level tests.
fn command_registry(
    commands: &Commands,
    ui: &RuntimeHandle,
    modules: Option<ModuleInstaller>,
    e2e_enabled: bool,
    e2e_broken: bool,
) -> Result<Registry, AlefError> {
    let mut registry = commands.to_registry(ui)?;
    ui.events().register(&mut registry)?;
    #[cfg(feature = "e2e")]
    e2e::register(&mut registry, ui, e2e_enabled, e2e_broken)?;
    #[cfg(not(feature = "e2e"))]
    let _ = (e2e_enabled, e2e_broken);
    if let Some(install) = modules {
        install(&mut registry, Arc::new(ui.clone()))?;
    }
    Ok(registry)
}

#[cfg(feature = "e2e")]
fn e2e_decision() -> (bool, bool) {
    let flag = |name| std::env::var(name).is_ok_and(|value| value == "1");
    (flag("ALEF_E2E"), flag("ALEF_E2E_BREAK"))
}

#[cfg(not(feature = "e2e"))]
fn e2e_decision() -> (bool, bool) {
    (false, false)
}

impl Bridge {
    pub async fn new(
        commands: Commands,
        assets: Option<&Path>,
        development_url: Option<Url>,
    ) -> io::Result<Self> {
        let assets = assets
            .filter(|_| development_url.is_none())
            .map(|selected| {
                Ok::<_, io::Error>(AssetInput::Disk {
                    application_dir: selected
                        .parent()
                        .ok_or_else(|| {
                            io::Error::new(
                                io::ErrorKind::InvalidInput,
                                "Missing application boundary",
                            )
                        })?
                        .to_owned(),
                    selected_root: selected.to_owned(),
                })
            })
            .transpose()?;
        let assets = match assets {
            Some(input) => Some(prepare_assets(input, None, &[]).await?),
            None => None,
        };
        Self::with_options(commands, assets, development_url, BridgeOptions::default()).await
    }

    /// `None` is supported only for development callers, which never resolve disk assets.
    pub async fn with_options(
        commands: Commands,
        assets: Option<PreparedAssets>,
        development_url: Option<Url>,
        options: BridgeOptions,
    ) -> io::Result<Self> {
        let entry = entry_path(options.entry.as_deref())?;
        if let Some(assets) = &assets {
            if assets.archive && (development_url.is_some() || crate::spikes::origin::enabled()) {
                return Err(io::Error::new(
                    io::ErrorKind::InvalidInput,
                    "Archive assets cannot use development or origin-spike URLs",
                ));
            }
        }
        let assets = if development_url.is_some() {
            None // Development never resolves, requires or serves disk, even for embedders.
        } else {
            let prepared =
                assets.ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "Missing assets"))?;
            if crate::spikes::origin::enabled() {
                if let Some(selected) = &prepared.selected_root {
                    crate::spikes::origin::set_assets(selected);
                }
            }
            Some(prepared.source)
        };
        let token = random_token()?;
        // Set in the origin arm; holding the guard before the fallible steps below means a
        // failed construction drops the site instead of leaving a stale global behind.
        let mut site_guard_slot: Option<assets::origin::SiteGuard> = None;
        let mut entry_url = match development_url {
            Some(base) => base
                .join(&entry)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?,
            None => {
                if let Some(base) = crate::spikes::origin::entry_url() {
                    base
                } else if let Some(label) = options.origin_host.as_deref() {
                    // `assets` is `Some` here: the `None` arm above only happens in dev mode.
                    let host = format!("{label}.alef");
                    let source = assets.as_ref().expect("origin assets").clone();
                    let base = Url::parse(&format!("https://{host}/"))
                        .and_then(|base| base.join(&entry))
                        .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?;
                    let site_guard = assets::origin::install(
                        host,
                        source,
                        options.csp.clone(),
                        options.allowed_external_origins.clone(),
                    );
                    site_guard_slot = Some(site_guard);
                    base
                } else {
                    Url::parse("native://app/")
                        .and_then(|base| base.join(&entry))
                        .expect("a validated root-relative entry")
                }
            }
        };
        let fragment = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("capability", &token)
            .finish();
        entry_url.set_fragment(Some(&fragment));
        let commands = Arc::new(commands);
        let (ui, requests) = RuntimeHandle::channel();
        let (e2e_enabled, e2e_broken) = e2e_decision();
        let registry = command_registry(&commands, &ui, options.modules, e2e_enabled, e2e_broken)
            .map_err(startup)?;
        let tokens: TokenSource =
            Arc::new(|| random_token().expect("the system must provide randomness"));
        let sessions = Arc::new(SessionManager::new(tokens, options.limits));
        let permissions = match options.permissions {
            Some(permissions) => permissions,
            None => closed_permissions()?,
        };
        let webcrypto_substitute = assets::origin::substitutes(&permissions);
        let config = TransportConfig {
            bootstrap_token: token.clone(),
            allowed_origins: options.allowed_origins,
            runtime_version: env!("CARGO_PKG_VERSION").to_owned(),
            limits: options.limits,
        };
        let transport = Arc::new(
            Transport::new(config, registry, sessions.clone(), permissions).map_err(startup)?,
        );
        let windows = WindowRegistry::default();
        // Origin mode serves assets through the https interception, never `native://app`.
        let origin_mode = entry_url.scheme() == "https";
        // Single source of truth: origin mode never serves `native://app` assets; the handler
        // and the test pin consume the same binding, so the test asserts the state the
        // handler actually uses.
        let native_assets = if origin_mode { None } else { assets.clone() };
        let handler = MemoryProtocol {
            assets: native_assets.clone(),
            handle: tokio::runtime::Handle::current(),
            transport: transport.clone(),
            windows: windows.clone(),
            limits: options.limits,
            csp: options.csp,
            log_calls: std::env::var("ALEF_LOG_CALLS").is_ok_and(|value| value == "1"),
        };
        let mut protocols = ProtocolRegistry::with_internal_protocols();
        protocols.register("native", handler).map_err(|error| {
            io::Error::other(format!("Protocol registration failed: {error:?}"))
        })?;
        Ok(Self {
            entry_url,
            registry: Some(protocols),
            commands,
            ui,
            requests: Some(requests),
            sessions,
            windows,
            transport,
            headless_loaded: options.headless_loaded,
            headless_tick: options.headless_tick,
            profile_dir: options.profile_dir,
            webcrypto_substitute,
            site: site_guard_slot,
            #[cfg(test)]
            native_assets,
        })
    }

    pub fn handle(&self) -> RuntimeHandle {
        self.ui.clone()
    }

    pub(crate) fn take_requests(&mut self) -> io::Result<mpsc::Receiver<UiRequest>> {
        self.requests
            .take()
            .ok_or_else(|| io::Error::other("Native window already attached"))
    }

    pub(crate) fn take_registry(&mut self) -> io::Result<ProtocolRegistry> {
        self.registry
            .take()
            .ok_or_else(|| io::Error::other("Bridge already attached to a window"))
    }

    pub(crate) fn sessions(&self) -> Arc<SessionManager> {
        self.sessions.clone()
    }

    pub(crate) fn windows(&self) -> WindowRegistry {
        self.windows.clone()
    }

    #[cfg(test)]
    pub(crate) fn transport(&self) -> Arc<Transport> {
        self.transport.clone()
    }

    #[cfg(test)]
    pub(crate) fn test_serves_native_assets(&self) -> bool {
        self.native_assets.is_some()
    }

    pub(crate) fn profile_dir(&self) -> Option<&Path> {
        self.profile_dir.as_deref()
    }

    /// Every document of this bridge gets the WebCrypto stand-in (the right was not granted).
    pub(crate) fn webcrypto_substitute(&self) -> bool {
        self.webcrypto_substitute
    }

    /// Call after the Servo window has closed. Fjall owners are dropped off the UI thread.
    pub async fn shutdown(self) -> io::Result<()> {
        // The guard clears the global only while it still points at this bridge's own site.
        drop(self.site);
        self.ui.detach();
        self.sessions.close_all().await;
        tokio::task::spawn_blocking(move || drop((self.registry, self.commands, self.transport)))
            .await
            .map_err(io::Error::other)
    }
}

pub(super) struct MemoryProtocol {
    pub(super) assets: Option<Arc<assets::Source>>,
    pub(super) handle: tokio::runtime::Handle,
    pub(super) transport: Arc<Transport>,
    pub(super) windows: WindowRegistry,
    pub(super) limits: Limits,
    pub(super) csp: Option<String>,
    /// `ALEF_LOG_CALLS=1`: one stderr line per transport request (route and status only; never
    /// headers, bodies or tokens), used by the end-to-end checks.
    pub(super) log_calls: bool,
}

impl ProtocolHandler for MemoryProtocol {
    fn is_fetchable(&self) -> bool {
        true
    }
    fn is_secure(&self) -> bool {
        true
    }

    fn load<'a>(
        &'a self,
        request: &'a mut Request,
        done_chan: &mut DoneChannel,
        _: &FetchContext,
    ) -> Pin<Box<dyn Future<Output = Response> + Send + 'a>> {
        // The body channel must be installed synchronously: the future cannot borrow it. Once it
        // is installed the fetch reads the body ONLY from it.
        let url = request.current_url();
        let transport_path = transport::transport_path(url.as_url());
        let transport_sender = transport_path
            .as_ref()
            .map(|_| transport::install(done_chan));
        let future = self.route_load(request, transport_path, transport_sender);
        #[cfg(feature = "e2e")]
        let future = transport::watch_request(future);
        Box::pin(future)
    }
}

impl MemoryProtocol {
    async fn asset_response(&self, request: &Request) -> Response {
        let mut response = Response::new(
            request.current_url().clone(),
            ResourceFetchTiming::new(request.timing_type()),
        );
        let url = request.current_url().clone();
        let result = if url.host_str() == Some("app") && request.method == http::Method::GET {
            let assets = self.assets.clone();
            let path = url.path().to_owned();
            let mut task = transport::OwnedTask(
                self.handle
                    .spawn(async move { assets::read_asset(assets, path).await }),
            );
            (&mut task.0)
                .await
                .map_err(io::Error::other)
                .and_then(|result| result)
        } else {
            Err(io::Error::new(
                io::ErrorKind::NotFound,
                "Unknown private resource",
            ))
        };
        let (status, bytes, content_type, permit) = match result {
            Ok(asset) => (200, asset.bytes, asset.mime, asset.permit),
            Err(error) => {
                let status = match error.kind() {
                    io::ErrorKind::PermissionDenied => 403,
                    io::ErrorKind::NotFound => 404,
                    io::ErrorKind::InvalidInput | io::ErrorKind::InvalidData => 400,
                    io::ErrorKind::WouldBlock => 429,
                    _ => 500,
                };
                #[cfg(feature = "e2e")]
                eprintln!(
                    "ALEF_E2E asset-refused status={status} path={} {error}",
                    url.path()
                );
                (
                    status,
                    serde_json::to_vec(&json!({"error": error.to_string()})).expect("JSON string"),
                    "application/json".to_owned(),
                    None,
                )
            }
        };
        response.status = HttpStatus::new_raw(status, vec![]);
        let csp =
            (url.host_str() == Some("app")).then(|| self.csp.as_deref().unwrap_or(DEFAULT_CSP));
        apply_headers(&mut response, &content_type, csp);
        *response.body.lock() = ResponseBody::Done(bytes);
        drop(permit); // Bytes now belong to Servo; renderer retention is outside admission.
        response
    }
}

/// Headers of asset responses; transport responses carry their own CORS policy.
fn apply_headers(response: &mut Response, content_type: &str, csp: Option<&str>) {
    response.headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(content_type).expect("MIME header"),
    );
    response.headers.insert(
        header::X_CONTENT_TYPE_OPTIONS,
        HeaderValue::from_static("nosniff"),
    );
    if let Some(csp) = csp {
        if let Ok(value) = HeaderValue::from_str(csp) {
            response
                .headers
                .insert(header::CONTENT_SECURITY_POLICY, value);
        }
    }
}

#[cfg(test)]
pub(crate) mod testing {
    use super::*;
    use alef_core::{registry::dispatch::Registry, session::Session};

    /// A closed permission set and an open session for registry-level tests.
    pub(crate) fn permissions() -> Arc<PermissionSet> {
        closed_permissions().expect("closed permissions")
    }

    pub(crate) async fn session(window: u64) -> Arc<Session> {
        let tokens: TokenSource = Arc::new(|| "test-token".to_owned());
        let sessions = SessionManager::new(tokens, Limits::default());
        sessions.begin_document(window).await
    }

    pub(crate) fn registry_of(commands: &Commands) -> Registry {
        let (ui, _requests) = RuntimeHandle::channel();
        command_registry(commands, &ui, None, false, false).expect("registry")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alef_core::protocol::transport::{Method, ResponseBody as Body, TransportRequest};
    use bytes::Bytes;
    use serde_json::Value;

    async fn bounded<T>(future: impl Future<Output = T>) -> T {
        tokio::time::timeout(std::time::Duration::from_secs(10), future)
            .await
            .expect("operation must complete within ten seconds")
    }

    fn disk_input(root_name: &str) -> (tempfile::TempDir, AssetInput) {
        let app = tempfile::tempdir().unwrap();
        let root = app.path().join(root_name);
        std::fs::create_dir_all(root.join("dir")).unwrap();
        for (name, bytes) in [
            ("index.html", "public"),
            ("app.js", "script"),
            ("dir/start.html", "entry"),
            ("dir/tool.html", "relative"),
        ] {
            std::fs::write(root.join(name), bytes).unwrap();
        }
        let input = AssetInput::Disk {
            application_dir: app.path().to_owned(),
            selected_root: root,
        };
        (app, input)
    }

    #[test]
    fn the_native_entry_is_root_relative_and_defaults_to_the_index_path() {
        assert_eq!(native_entry(None).unwrap().as_str(), "native://app/");
        assert_eq!(
            native_entry(Some("/dir/start.html?x=1#part"))
                .unwrap()
                .as_str(),
            "native://app/dir/start.html?x=1#part"
        );
        for entry in [
            "relative.html",
            "//other/index.html",
            r"/dir\index.html",
            "https://evil/index.html",
        ] {
            assert_eq!(
                native_entry(Some(entry)).unwrap_err().kind(),
                io::ErrorKind::InvalidInput,
                "{entry}"
            );
        }
    }

    #[tokio::test]
    async fn prepared_documents_keep_their_path_context_and_check_every_relative_reference() {
        for (entry, documents, kind, path) in [
            (
                Some("/missing.html"),
                vec![],
                io::ErrorKind::NotFound,
                Some("/missing.html"),
            ),
            (
                None,
                vec!["/app.js", "/second.html"],
                io::ErrorKind::NotFound,
                Some("/second.html"),
            ),
            (
                None,
                vec!["/a%2fb"],
                io::ErrorKind::InvalidInput,
                Some("/a%2fb"),
            ),
            (
                None,
                vec!["native://app:123/app.js"],
                io::ErrorKind::InvalidInput,
                None,
            ),
            (
                None,
                vec!["native://other/app.js"],
                io::ErrorKind::InvalidInput,
                None,
            ),
            (
                None,
                vec!["https://app/app.js"],
                io::ErrorKind::InvalidInput,
                None,
            ),
        ] {
            let documents: Vec<String> = documents.into_iter().map(str::to_owned).collect();
            let (_app, input) = disk_input("dist");
            let error = bounded(prepare_assets(input, entry, &documents))
                .await
                .err()
                .expect("the document must fail");
            assert_eq!(error.kind(), kind);
            if let Some(path) = path {
                assert!(
                    error.to_string().starts_with(&format!("{path}: ")),
                    "{error}"
                );
            }
        }
        let (_app, input) = disk_input("dist");
        let prepared = bounded(prepare_assets(
            input,
            Some("/dir/start.html?x=1#part"),
            &["tool.html?y=2#part".to_owned()],
        ))
        .await
        .expect("the reference resolves against the entry directory");
        assert!(!prepared.archive);
        assert_eq!(
            prepared.source.read("/dir/tool.html").unwrap().0,
            b"relative"
        );
    }

    #[tokio::test]
    async fn prepared_disk_assets_keep_the_selected_root_and_are_not_preflighted_twice() {
        let (app, root) = {
            let app = tempfile::tempdir().unwrap();
            let root = app.path().join("web/build");
            std::fs::create_dir_all(&root).unwrap();
            std::fs::write(root.join("index.html"), b"public").unwrap();
            (app, root)
        };
        let prepared = bounded(prepare_assets(
            AssetInput::Disk {
                application_dir: app.path().to_owned(),
                selected_root: root.clone(),
            },
            None,
            &[],
        ))
        .await
        .expect("prepared assets");
        assert!(!prepared.archive);
        assert_eq!(prepared.selected_root.as_ref(), Some(&root));
        std::fs::remove_file(root.join("index.html")).unwrap();
        assert_eq!(
            prepared.source.read("/").unwrap_err().kind(),
            io::ErrorKind::NotFound
        );
        let bridge = bounded(Bridge::with_options(
            Commands::new(),
            Some(prepared),
            None,
            BridgeOptions::default(),
        ))
        .await
        .expect("the already audited source is handed over");
        bounded(bridge.shutdown()).await.unwrap();
    }

    fn registry_with_hooks(enabled: bool) -> Registry {
        let mut commands = Commands::new();
        commands
            .register("app.probe", |_: Value, _| async { Ok(true) })
            .expect("embedder command");
        let modules: ModuleInstaller = Box::new(|registry, _host| {
            registry
                .command::<Value>("module.probe")?
                .handler(
                    |_, args| async move { Ok(alef_core::registry::command::Reply::Json(args)) },
                )
        });
        let (ui, _requests) = RuntimeHandle::channel();
        command_registry(&commands, &ui, Some(modules), enabled, false).expect("startup registry")
    }

    #[test]
    fn shared_registry_keeps_embedder_events_and_modules_without_e2e_mode() {
        let names = registry_with_hooks(false).command_names();
        assert!(names.iter().any(|name| name == "app.probe"));
        assert!(names.iter().any(|name| name == "module.probe"));
        assert!(names.iter().any(|name| name == "runtime.events.subscribe"));
        assert!(!names.iter().any(|name| name.starts_with("e2e.")));
    }

    #[cfg(not(feature = "e2e"))]
    #[test]
    fn feature_off_registry_has_no_e2e_commands_even_when_requested() {
        let names = registry_with_hooks(true).command_names();
        assert!(!names.iter().any(|name| name.starts_with("e2e.")));
    }

    #[cfg(feature = "e2e")]
    #[test]
    fn feature_on_registry_installs_all_e2e_commands_when_enabled() {
        let names: Vec<_> = registry_with_hooks(true)
            .command_names()
            .into_iter()
            .filter(|name| name.starts_with("e2e."))
            .collect();
        assert_eq!(
            names,
            [
                "e2e.appEnv",
                "e2e.dropFiles",
                "e2e.echo",
                "e2e.flood",
                "e2e.fsRead",
                "e2e.fsWrite",
                "e2e.once",
                "e2e.report",
            ]
        );
    }

    fn asset_response(content_type: &str, csp: Option<&str>) -> Response {
        let mut response = Response::new(
            servo::ServoUrl::parse("native://app/index.html").expect("asset URL"),
            ResourceFetchTiming::new(net_traits::ResourceTimingType::None),
        );
        apply_headers(&mut response, content_type, csp);
        response
    }

    // R-006: each assertion fails with its legacy transport header restored.
    #[test]
    fn asset_headers_do_not_allow_cross_origin_reads() {
        let response = asset_response("text/html", Some(DEFAULT_CSP));
        assert_eq!(
            response.headers.get(header::ACCESS_CONTROL_ALLOW_ORIGIN),
            None
        );
    }

    #[test]
    fn asset_headers_do_not_advertise_transport_methods() {
        let response = asset_response("application/json", Some(DEFAULT_CSP));
        assert_eq!(
            response.headers.get(header::ACCESS_CONTROL_ALLOW_METHODS),
            None
        );
    }

    #[test]
    fn asset_headers_do_not_advertise_transport_authorization() {
        let response = asset_response("application/octet-stream", None);
        assert_eq!(
            response.headers.get(header::ACCESS_CONTROL_ALLOW_HEADERS),
            None
        );
    }

    #[test]
    fn asset_headers_preserve_content_type_nosniff_and_csp() {
        for (content_type, csp) in [
            ("text/html", Some(DEFAULT_CSP)),
            ("application/json", Some("default-src 'none'")),
            ("application/octet-stream", None),
        ] {
            let response = asset_response(content_type, csp);
            assert_eq!(response.headers[header::CONTENT_TYPE], content_type);
            assert_eq!(response.headers[header::X_CONTENT_TYPE_OPTIONS], "nosniff");
            assert_eq!(
                response.headers.get(header::CONTENT_SECURITY_POLICY),
                csp.map(|value| HeaderValue::from_str(value).expect("CSP"))
                    .as_ref(),
            );
        }
    }

    fn capability(bridge: &Bridge) -> String {
        let fragment = bridge.entry_url.fragment().expect("fragment");
        url::form_urlencoded::parse(fragment.as_bytes())
            .find(|(key, _)| key == "capability")
            .map(|(_, value)| value.into_owned())
            .expect("capability")
    }

    fn json_of(response: &alef_core::protocol::transport::TransportResponse) -> Value {
        match &response.body {
            Body::Bytes(bytes) => serde_json::from_slice(bytes).expect("json"),
            other => panic!("expected bytes, got {other:?}"),
        }
    }

    fn post(path: &str, token: &str, args: &Value) -> TransportRequest {
        TransportRequest {
            method: Method::Post,
            path: path.to_owned(),
            headers: vec![
                ("authorization".into(), format!("Bearer {token}")),
                ("content-type".into(), "application/json".into()),
            ],
            body: Bytes::from(serde_json::to_vec(args).expect("json")),
            window: 1,
            document: None,
        }
    }

    #[tokio::test]
    async fn legacy_commands_work_through_hello_and_call_with_a_per_document_token() {
        let mut commands = Commands::new();
        commands
            .register("hello", |name: String, _ui| async move {
                Ok(format!("hi {name}"))
            })
            .expect("register");
        let bridge = Bridge::new(
            commands,
            None,
            Some(Url::parse("http://127.0.0.1:1/").unwrap()),
        )
        .await
        .expect("bridge");
        let transport = bridge.transport();
        let bootstrap = capability(&bridge);
        assert_eq!(
            transport
                .handle(post("call/runtime.hello", &bootstrap, &json!({})))
                .await
                .status,
            404,
            "no document yet"
        );
        let session = bridge.sessions().begin_document(1).await;
        let hello = transport
            .handle(post("call/runtime.hello", &bootstrap, &json!({})))
            .await;
        assert_eq!(hello.status, 200);
        let info = json_of(&hello);
        assert_eq!(info["token"], session.token());
        assert_eq!(info["modules"], json!(["app", "runtime", "window"]));
        let reply = transport
            .handle(post("call/app.hello", session.token(), &json!("Alef")))
            .await;
        assert_eq!(json_of(&reply), json!("hi Alef"));
        let with_bootstrap = transport
            .handle(post("call/app.hello", &bootstrap, &json!("Alef")))
            .await;
        assert_eq!(
            with_bootstrap.status, 403,
            "the bootstrap token is not a session token"
        );
        bridge.shutdown().await.expect("shutdown");
    }
}
