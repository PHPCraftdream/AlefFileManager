// SPDX-License-Identifier: MIT OR Apache-2.0
//! Production `https://<label>.alef` application origin: interception, refusal of external
//! hosts, and asset serving through the same [`Source`] as `native://app`.
use std::io;
use std::sync::{Arc, Mutex};

use http::header::{CONTENT_SECURITY_POLICY, CONTENT_TYPE, X_CONTENT_TYPE_OPTIONS};
use http::{HeaderMap, HeaderValue, Method, StatusCode};
use servo::{Preferences, WebResourceLoad, WebResourceResponse};
use url::Url;

// `consent::Decision` is referred to by full path: origin's own `Decision` enum takes the name.
use alef_core::security::permissions::{Grants, Permission, PermissionSet};

const CHUNK_BYTES: usize = 256 * 1024;

/// One pipeline command for [`ServoChannel`]: the intercepted handle is not nameable
/// (`InterceptedWebResourceLoad` is not re-exported by servo 0.6.0), so it lives inside
/// a closure driven by this command.
enum Pipeline {
    Body(Vec<u8>),
    Finish,
}

/// The head of an interception response: status and headers, type-agnostic.
#[derive(Clone)]
struct ResponseHead {
    status: StatusCode,
    headers: HeaderMap,
}

/// Where a started interception response goes: `start`, then body chunks, then `finish` —
/// in this order, and never a cancel (the trait has no cancel operation).
trait ResponseChannel {
    fn start(&mut self, head: ResponseHead);
    fn body(&mut self, chunk: Vec<u8>);
    fn finish(&mut self);
}

/// A short HTTP reason phrase per status, as the spike used; the JSON error stays in the body.
fn reason(status: StatusCode) -> &'static [u8] {
    match status.as_u16() {
        400 => b"Bad request",
        403 => b"Forbidden",
        404 => b"Not found",
        405 => b"Method not allowed",
        429 => b"Too many requests",
        _ => b"Rejected by origin",
    }
}

/// The production channel: starts the interception, streams the body to Servo, finishes it.
struct ServoChannel {
    load: Option<WebResourceLoad>,
    url: Url,
    #[allow(clippy::type_complexity)]
    pipeline: Option<Box<dyn FnMut(Pipeline)>>,
}

impl ResponseChannel for ServoChannel {
    fn start(&mut self, head: ResponseHead) {
        let load = self.load.take().expect("the channel starts once");
        let response = WebResourceResponse::new(self.url.clone())
            .status_code(head.status)
            .status_message(reason(head.status).to_vec())
            .headers(head.headers);
        let mut intercepted = Some(load.intercept(response));
        self.pipeline = Some(Box::new(move |command| match command {
            Pipeline::Body(chunk) => intercepted
                .as_mut()
                .expect("body after finish")
                .send_body_data(chunk),
            Pipeline::Finish => intercepted.take().expect("finish twice").finish(),
        }));
    }
    fn body(&mut self, chunk: Vec<u8>) {
        (self.pipeline.as_mut().expect("start before body"))(Pipeline::Body(chunk));
    }
    fn finish(&mut self) {
        (self.pipeline.as_mut().expect("start before finish"))(Pipeline::Finish);
    }
}

fn asset_head(mime: &str, csp: &str) -> ResponseHead {
    let mut headers = HeaderMap::new();
    headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_str(mime).expect("MIME header"),
    );
    headers.insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    headers.insert(
        CONTENT_SECURITY_POLICY,
        HeaderValue::from_str(csp).expect("CSP header"),
    );
    ResponseHead {
        status: StatusCode::OK,
        headers,
    }
}

fn error_head(status: u16, csp: &str) -> ResponseHead {
    let mut headers = HeaderMap::new();
    headers.insert(CONTENT_TYPE, HeaderValue::from_static("application/json"));
    headers.insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    headers.insert(
        CONTENT_SECURITY_POLICY,
        HeaderValue::from_str(csp).expect("CSP header"),
    );
    ResponseHead {
        status: StatusCode::from_u16(status).expect("valid status"),
        headers,
    }
}

fn error_body() -> Vec<u8> {
    serde_json::to_vec(&serde_json::json!({ "error": "Rejected by origin" })).expect("JSON body")
}

/// Hands one response over the channel; the admission permit (if any) is dropped only after
/// `finish`, so the slot is held until the body has been handed over completely.
fn respond(
    channel: &mut impl ResponseChannel,
    head: ResponseHead,
    body: Vec<u8>,
    permit: Option<tokio::sync::OwnedSemaphorePermit>,
) {
    channel.start(head);
    for chunk in body.chunks(CHUNK_BYTES) {
        channel.body(chunk.to_vec());
    }
    channel.finish();
    drop(permit);
}

/// The https analogue of `DEFAULT_CSP`: `'self'` covers the https origin, `native:` stays
/// allowed for the transport.
pub(crate) const DEFAULT_ORIGIN_CSP: &str = "default-src 'self'; connect-src 'self' native:; img-src 'self' data:; style-src 'self' 'unsafe-inline'; object-src 'none'; frame-src 'none'; base-uri 'none'";

pub(crate) struct Site {
    pub(crate) host: String,
    /// `https://{host}`: the full origin the entry URL and `classify` compare.
    pub(crate) origin: String,
    /// ASCII origin serializations the manifest's `external` section allows (lowercase).
    pub(crate) allowed: Vec<String>,
    pub(crate) assets: Arc<super::Source>,
    pub(crate) csp: String,
}

// std Mutex so tests can reset it; recover from poisoning, the site is plain data.
static SITE: Mutex<Option<Arc<Site>>> = Mutex::new(None);

/// Serializes the site-sensitive tests (here and in assets/tests.rs) against each other.
#[cfg(test)]
pub(crate) static SITE_TEST_LOCK: tokio::sync::Mutex<()> = tokio::sync::Mutex::const_new(());

fn set_site(site: Option<Arc<Site>>) {
    *lock() = site;
}

/// Owns the installed site: dropping it clears the global ONLY while the global still points
/// at this very site, so one bridge's shutdown can never drop another's site and a failed
/// bridge construction cannot leave a stale one behind.
pub(crate) struct SiteGuard(Arc<Site>);

impl std::ops::Drop for SiteGuard {
    fn drop(&mut self) {
        let mut installed = lock();
        if installed
            .as_ref()
            .is_some_and(|site| Arc::ptr_eq(site, &self.0))
        {
            *installed = None;
        }
    }
}

/// One Servo option set for every builder: per-application profile root, temporary storage,
/// single process.
pub(crate) fn servo_opts(profile_dir: Option<std::path::PathBuf>) -> servo::Opts {
    servo::Opts {
        multiprocess: false,
        temporary_storage: true,
        config_dir: profile_dir,
        ..Default::default()
    }
}

/// Complete application host including the `.alef` suffix; must equal the entry URL's host.
pub(crate) fn install(
    host: String,
    assets: Arc<super::Source>,
    csp: Option<String>,
    allowed: Vec<String>,
) -> SiteGuard {
    let origin = format!("https://{host}");
    let site = Arc::new(Site {
        host,
        origin,
        allowed,
        assets,
        csp: csp.unwrap_or_else(|| DEFAULT_ORIGIN_CSP.into()),
    });
    set_site(Some(site.clone()));
    SiteGuard(site)
}

fn lock() -> std::sync::MutexGuard<'static, Option<Arc<Site>>> {
    SITE.lock().unwrap_or_else(|error| error.into_inner())
}

fn site_snapshot() -> Option<Arc<Site>> {
    lock().clone()
}

#[cfg(test)]
pub(crate) fn test_site_host() -> Option<String> {
    site_snapshot().map(|site| site.host.clone())
}

/// IndexedDB needs the pref; the secure context is guaranteed by the https origin.
pub(crate) fn preferences() -> Preferences {
    Preferences {
        dom_indexeddb_enabled: true,
        ..Default::default()
    }
}

/// URL for logs without the fragment: the entry fragment carries the bridge capability.
fn redact(url: &Url) -> String {
    let mut clean = url.clone();
    clean.set_fragment(None);
    clean.to_string()
}

/// Response decision by method; pure so unit tests need no Servo types.
#[derive(Debug, PartialEq)]
pub(crate) enum Decision {
    Serve,
    Reject(u16),
}

fn decide(method: &Method) -> Decision {
    match *method {
        Method::GET => Decision::Serve,
        _ => Decision::Reject(405),
    }
}

/// The same table as `MemoryProtocol::asset_response` in bridge/mod.rs.
fn status_for(error: &io::Error) -> u16 {
    match error.kind() {
        io::ErrorKind::PermissionDenied => 403,
        io::ErrorKind::NotFound => 404,
        io::ErrorKind::InvalidInput | io::ErrorKind::InvalidData => 400,
        io::ErrorKind::WouldBlock => 429,
        _ => 500,
    }
}

/// How a request relates to the installed site; pure so unit tests need no Servo types.
#[derive(Debug, PartialEq)]
enum Classification {
    Untouched,
    Cancelled,
    App,
}

fn classify(
    site_origin: &str,
    allowed: &[String],
    request_origin: &str,
    scheme: &str,
    request_host: Option<&str>,
) -> Classification {
    match (scheme, request_host) {
        // The full origin (scheme + host + port) must match: a different port is a different origin.
        ("https", Some(_)) if request_origin == site_origin => Classification::App,
        ("http" | "https", Some(_)) => {
            if allowed
                .iter()
                .any(|source| alef_core::security::csp::origin_matches(source, request_origin))
            {
                Classification::Untouched
            } else {
                Classification::Cancelled
            }
        }
        _ => Classification::Untouched,
    }
}

/// Reads the asset through the shared admission queue and lookup; `Err` is an HTTP status.
/// Returns the shared struct so the admission permit survives until the body handoff.
pub(crate) async fn read(
    site: &Site,
    method: &Method,
    path: &str,
) -> Result<super::AssetBytes, u16> {
    if let Decision::Reject(status) = decide(method) {
        return Err(status);
    }
    super::read_asset(Some(site.assets.clone()), path.to_owned())
        .await
        .map_err(|error| status_for(&error))
}

/// Runs on the main thread: Servo dispatches `NetToEmbedderMsg` from `spin_event_loop`.
/// `WebResourceLoad`/`InterceptedWebResourceLoad` are not `Send` (Box<dyn AbstractSender>),
/// so interception is completed here; only the disk read runs on Tokio.
pub(crate) fn web_resource(load: WebResourceLoad) {
    if crate::spikes::origin::enabled() {
        crate::spikes::origin::web_resource(load);
        return;
    }
    // No site means dev-URL or native mode: dropping the load is DoNotIntercept.
    let Some(site) = site_snapshot() else {
        return;
    };
    let request = load.request.clone();
    let url = request.url.clone();
    eprintln!("origin: {} {}", request.method, redact(&url));
    match classify(
        &site.origin,
        &site.allowed,
        &url.origin().ascii_serialization(),
        url.scheme(),
        url.host_str(),
    ) {
        Classification::Untouched => return, // native:, about:, plain http, etc.
        Classification::Cancelled => {
            let started = std::time::Instant::now();
            // cancel() lives on InterceptedWebResourceLoad; intercept first, then cancel.
            load.intercept(WebResourceResponse::new(url)).cancel();
            eprintln!(
                "origin: external request cancelled in {:?}",
                started.elapsed()
            );
            return;
        }
        Classification::App => {}
    }
    match decide(&request.method) {
        Decision::Reject(status) => {
            eprintln!("origin: rejected {status} {}", redact(&url));
            reject(load, url, status, &site.csp);
        }
        Decision::Serve => serve(load, url, site),
    }
}

// The interception is never cancelled on an error: like the Failed arm, the rejection is a
// complete HTTP response, not a network failure.
fn reject(load: WebResourceLoad, url: Url, status: u16, csp: &str) {
    let mut channel = ServoChannel {
        load: Some(load),
        url,
        pipeline: None,
    };
    respond(&mut channel, error_head(status, csp), error_body(), None);
}

fn serve(load: WebResourceLoad, url: Url, site: Arc<Site>) {
    let (sender, receiver) = std::sync::mpsc::channel();
    enum AssetMessage {
        Ok(super::AssetBytes),
        Failed(u16),
    }
    let task_site = site.clone();
    let task_path = url.path().to_owned();
    let task_method = load.request.method.clone();
    tokio::runtime::Handle::current().spawn(async move {
        let message = match read(&task_site, &task_method, &task_path).await {
            Ok(asset) => AssetMessage::Ok(asset),
            Err(status) => AssetMessage::Failed(status),
        };
        let _ = sender.send(message);
    });
    let started = std::time::Instant::now();
    // Read first: the response headers (mime) are only known after the lookup.
    match receiver.recv() {
        Ok(AssetMessage::Ok(asset)) => {
            // Capture the length first: the bytes move into the response.
            let super::AssetBytes {
                bytes,
                mime,
                permit,
            } = asset;
            let served = bytes.len();
            let mut channel = ServoChannel {
                load: Some(load),
                url,
                pipeline: None,
            };
            respond(&mut channel, asset_head(&mime, &site.csp), bytes, permit);
            eprintln!("origin: served {served} B in {:?}", started.elapsed());
        }
        Ok(AssetMessage::Failed(status)) => {
            eprintln!("origin: read failed with HTTP {status}");
            // Errors are HTTP responses, never cancel.
            let mut channel = ServoChannel {
                load: Some(load),
                url,
                pipeline: None,
            };
            respond(
                &mut channel,
                error_head(status, &site.csp),
                error_body(),
                None,
            );
        }
        Err(_) => {
            eprintln!("origin: reader task vanished"); // Drop finishes the interception.
        }
    }
}

/// WebCrypto substitution: when the `webcrypto` right is not granted, every document gets a
/// `crypto.subtle` whose methods reject, while `getRandomValues`/`randomUUID` stay real.

/// Per-realm substitution: every function of `SubtleCrypto.prototype` (enumerated at install
/// time, so encapsulate/decapsulate and future methods are covered) is replaced by a sealed stub
/// that returns a rejected promise; the originals are dropped, so neither `crypto.subtle` nor the
/// prototype getter reaches the engine. Each step logs its own failure and the others still run.
/// Workers and realms created before this script runs are NOT covered (residual risk).
pub(crate) const SUBTITUTE_SCRIPT: &str = r#"
(function () {
    "use strict";
    var MESSAGE = "crypto.subtle is substituted by Alef: the webcrypto right was not granted";
    function fail(step, error) {
        try { console.error("webcrypto: substitution step failed: " + step, error); } catch (ignored) {}
    }
    function reject() { return Promise.reject(new DOMException(MESSAGE, "NotSupportedError")); }
    var proto = null;
    try {
        if (typeof SubtleCrypto === "function") proto = SubtleCrypto.prototype;
        else if (self.crypto && self.crypto.subtle) proto = Object.getPrototypeOf(self.crypto.subtle);
    } catch (error) { fail("lookup", error); }
    if (!proto) return;
    var names = [];
    try { names = Object.getOwnPropertyNames(proto); } catch (error) { fail("enumerate", error); }
    names.forEach(function (name) {
        if (name === "constructor") return;
        try {
            var descriptor = Object.getOwnPropertyDescriptor(proto, name);
            if (!descriptor || typeof descriptor.value !== "function") return;
            Object.defineProperty(proto, name, {
                value: reject, writable: false, enumerable: descriptor.enumerable, configurable: false
            });
        } catch (error) { fail(name, error); }
    });
})();
"#;

/// True unless the user allowed the engine WebCrypto: substitute, deny, and undeclared all stand in.
pub(crate) fn substitutes(permissions: &PermissionSet) -> bool {
    match permissions.check(Permission::WebCrypto, None, &Grants::default()) {
        Ok(alef_core::security::consent::Decision::Allow) => false,
        _ => true,
    }
}

#[cfg(test)]
mod tests;
#[cfg(test)]
mod webcrypto_tests;
