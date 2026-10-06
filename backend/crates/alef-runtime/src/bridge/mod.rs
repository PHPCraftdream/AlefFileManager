// SPDX-License-Identifier: MIT OR Apache-2.0
//! The `native://` protocol: application assets and the transport routes `call`/`stream`.
pub(crate) mod commands;
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

/// Content-Security-Policy of `native://app` documents unless the embedder supplies its own.
const DEFAULT_CSP: &str = "default-src native:; connect-src native:; img-src native: data:; style-src native: 'unsafe-inline'; object-src 'none'; frame-src 'none'; base-uri 'none'";

/// Embedder-supplied policy of the bridge; the default suits the File Manager.
#[derive(Default)]
pub struct BridgeOptions {
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
        cli: CliPermissions { exec: Vec::new() },
        net: NetPermissions {
            http: Vec::new(),
            socket: Vec::new(),
        },
        shell: ShellPermissions {
            open_external: Vec::new(),
        },
        clipboard: ClipboardPermissions { read: false },
        shortcut: ShortcutPermissions { global: false },
        secrets: false,
        app: AppPermissions { env: Vec::new() },
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
        app: root,
    };
    PermissionSet::from_manifest(&policy, &vars)
        .map(Arc::new)
        .map_err(startup)
}

impl Bridge {
    pub async fn new(
        commands: Commands,
        assets: Option<&Path>,
        development_url: Option<Url>,
    ) -> io::Result<Self> {
        Self::with_options(commands, assets, development_url, BridgeOptions::default()).await
    }

    pub async fn with_options(
        commands: Commands,
        assets: Option<&Path>,
        development_url: Option<Url>,
        options: BridgeOptions,
    ) -> io::Result<Self> {
        let entry = entry_path(options.entry.as_deref())?;
        let assets = match assets {
            Some(path) => {
                let path = tokio::fs::canonicalize(path).await?;
                let document = match entry.trim_matches('/') {
                    "" => "index.html",
                    other => other,
                };
                if !tokio::fs::try_exists(path.join(document)).await? {
                    return Err(io::Error::new(
                        io::ErrorKind::NotFound,
                        format!(
                            "The entry document {document} is missing in the application assets"
                        ),
                    ));
                }
                if crate::spikes::origin::enabled() {
                    crate::spikes::origin::set_assets(&path);
                }
                Some(path)
            }
            None => None,
        };
        let token = random_token()?;
        let mut entry_url = match development_url {
            Some(base) => base
                .join(&entry)
                .map_err(|error| io::Error::new(io::ErrorKind::InvalidInput, error))?,
            None => crate::spikes::origin::entry_url().unwrap_or_else(|| {
                Url::parse("native://app/")
                    .and_then(|base| base.join(&entry))
                    .expect("a validated root-relative entry")
            }),
        };
        let fragment = url::form_urlencoded::Serializer::new(String::new())
            .append_pair("capability", &token)
            .finish();
        entry_url.set_fragment(Some(&fragment));
        let commands = Arc::new(commands);
        let (ui, requests) = RuntimeHandle::channel();
        let mut registry = commands.to_registry(&ui).map_err(startup)?;
        ui.events().register(&mut registry).map_err(startup)?;
        e2e::register(&mut registry, &ui).map_err(startup)?;
        let tokens: TokenSource =
            Arc::new(|| random_token().expect("the system must provide randomness"));
        let sessions = Arc::new(SessionManager::new(tokens, options.limits));
        let permissions = match options.permissions {
            Some(permissions) => permissions,
            None => closed_permissions()?,
        };
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
        let handler = MemoryProtocol {
            assets,
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

    /// Call after the Servo window has closed. Fjall owners are dropped off the UI thread.
    pub async fn shutdown(self) -> io::Result<()> {
        self.ui.detach();
        self.sessions.close_all().await;
        tokio::task::spawn_blocking(move || drop((self.registry, self.commands, self.transport)))
            .await
            .map_err(io::Error::other)
    }
}

pub(super) struct MemoryProtocol {
    pub(super) assets: Option<PathBuf>,
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
        Box::pin(async move {
            if let (Some(path), Some(sender)) = (transport_path, transport_sender) {
                return self.transport(request, path, sender).await;
            }
            let mut response = Response::new(
                request.current_url().clone(),
                ResourceFetchTiming::new(request.timing_type()),
            );
            let url = request.current_url().clone();
            let result = if url.host_str() == Some("app") && request.method == http::Method::GET {
                let assets = self.assets.clone();
                let path = url.path().trim_start_matches('/').to_owned();
                let mut task = transport::OwnedTask(
                    self.handle
                        .spawn(async move { read_asset(assets, path).await }),
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
            let (status, bytes, content_type) = match result {
                Ok((bytes, content_type)) => (200, bytes, content_type),
                Err(error) => {
                    let status = match error.kind() {
                        io::ErrorKind::PermissionDenied => 403,
                        io::ErrorKind::NotFound => 404,
                        io::ErrorKind::InvalidInput | io::ErrorKind::InvalidData => 400,
                        io::ErrorKind::WouldBlock => 429,
                        _ => 500,
                    };
                    (
                        status,
                        serde_json::to_vec(&json!({"error": error.to_string()}))
                            .expect("JSON string"),
                        "application/json".to_owned(),
                    )
                }
            };
            response.status = HttpStatus::new_raw(status, vec![]);
            let csp =
                (url.host_str() == Some("app")).then(|| self.csp.as_deref().unwrap_or(DEFAULT_CSP));
            apply_headers(&mut response, &content_type, csp);
            *response.body.lock() = ResponseBody::Done(bytes);
            response
        })
    }
}

async fn read_asset(assets: Option<PathBuf>, requested: String) -> io::Result<(Vec<u8>, String)> {
    let root = assets
        .ok_or_else(|| io::Error::new(io::ErrorKind::NotFound, "Assets are served by Rsbuild"))?;
    let requested = if requested.is_empty() {
        "index.html"
    } else {
        &requested
    };
    let path = tokio::fs::canonicalize(root.join(requested)).await?;
    if !path.starts_with(&root) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Asset outside frontend root",
        ));
    }
    let mime = mime_guess::from_path(&path)
        .first_or_octet_stream()
        .to_string();
    Ok((tokio::fs::read(path).await?, mime))
}

/// Headers of the legacy/asset/spike responses (transport responses carry their own).
fn apply_headers(response: &mut Response, content_type: &str, csp: Option<&str>) {
    response.headers.insert(
        header::CONTENT_TYPE,
        HeaderValue::from_str(content_type).expect("MIME header"),
    );
    response.headers.insert(
        header::ACCESS_CONTROL_ALLOW_ORIGIN,
        HeaderValue::from_static("*"),
    );
    response.headers.insert(
        header::ACCESS_CONTROL_ALLOW_METHODS,
        HeaderValue::from_static("POST, OPTIONS"),
    );
    response.headers.insert(
        header::ACCESS_CONTROL_ALLOW_HEADERS,
        HeaderValue::from_static("authorization, content-type"),
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
        commands.to_registry(&ui).expect("registry")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alef_core::protocol::transport::{Method, ResponseBody as Body, TransportRequest};
    use bytes::Bytes;
    use serde_json::Value;

    #[tokio::test]
    async fn refuses_asset_parent_traversal() {
        let directory = tempfile::tempdir().expect("assets");
        let root = directory.path().join("frontend");
        tokio::fs::create_dir(&root).await.expect("frontend");
        tokio::fs::write(directory.path().join("secret"), b"private")
            .await
            .expect("outside asset");
        let root = tokio::fs::canonicalize(root).await.expect("root");
        let error = read_asset(Some(root), "../secret".to_owned())
            .await
            .expect_err("outside root");
        assert_eq!(error.kind(), io::ErrorKind::PermissionDenied);
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
