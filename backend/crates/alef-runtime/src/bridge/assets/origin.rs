// SPDX-License-Identifier: MIT OR Apache-2.0
//! Production `https://<label>.alef` application origin: interception, refusal of external
//! hosts, and asset serving through the same [`Source`] as `native://app`.
use std::io;
use std::sync::{Arc, Mutex};

use http::header::{CONTENT_SECURITY_POLICY, CONTENT_TYPE, X_CONTENT_TYPE_OPTIONS};
use http::{HeaderMap, HeaderValue, Method, StatusCode};
use servo::{Preferences, WebResourceLoad, WebResourceResponse};
use url::Url;

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
            let super::super::AssetBytes {
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::future::Future;

    async fn bounded<T>(future: impl Future<Output = T>) -> T {
        tokio::time::timeout(std::time::Duration::from_secs(10), future)
            .await
            .expect("operation must complete within ten seconds")
    }

    /// The disk fixture `site_from_disk` builds its site from; shared with the RAII tests.
    fn disk_source() -> (tempfile::TempDir, Arc<super::super::Source>) {
        let app = tempfile::tempdir().expect("temporary directory");
        let root = app.path().join("dist");
        std::fs::create_dir_all(&root).expect("root dir");
        std::fs::write(root.join("index.html"), b"public").expect("index");
        std::fs::write(root.join("app.js"), b"script").expect("script");
        let source = Arc::new(
            super::super::Source::prepare(super::super::super::AssetInput::Disk {
                application_dir: app.path().to_owned(),
                selected_root: root,
            })
            .expect("prepared source"),
        );
        (app, source)
    }

    fn site_from_disk() -> (tempfile::TempDir, Arc<Site>) {
        let (app, source) = disk_source();
        (
            app,
            Arc::new(Site {
                host: "org2eexample2eapp.alef".into(),
                origin: "https://org2eexample2eapp.alef".into(),
                allowed: Vec::new(),
                assets: source,
                csp: DEFAULT_ORIGIN_CSP.into(),
            }),
        )
    }

    #[tokio::test]
    async fn a_get_request_serves_the_file_from_the_source() {
        let (_app, site) = site_from_disk();
        let asset = bounded(read(&site, &Method::GET, "/app.js"))
            .await
            .expect("existing asset");
        assert_eq!(asset.bytes, b"script");
        assert_eq!(asset.mime, "text/javascript");
        let asset = bounded(read(&site, &Method::GET, "/"))
            .await
            .expect("index");
        assert_eq!(asset.bytes, b"public");
    }

    // POST reaches `decide` through the same `read` path `web_resource` uses, not only the
    // pure helper: the rejection must also hold without the interception layer.
    #[tokio::test]
    async fn a_post_request_is_rejected_through_the_shared_read_path() {
        let (_app, site) = site_from_disk();
        assert_eq!(
            bounded(read(&site, &Method::POST, "/app.js"))
                .await
                .unwrap_err(),
            405
        );
    }

    #[tokio::test]
    async fn a_missing_file_maps_to_not_found() {
        let (_app, site) = site_from_disk();
        assert_eq!(
            bounded(read(&site, &Method::GET, "/absent.js"))
                .await
                .unwrap_err(),
            404
        );
    }

    // The URL parser normalizes dot segments, so a parsed URL never carries `..` into the
    // lookup: every escape attempt ends as a plain miss inside the root.
    #[tokio::test]
    async fn dot_dot_urls_never_serve_outside_the_root() {
        let app = tempfile::tempdir().expect("temporary directory");
        let root = app.path().join("dist");
        std::fs::create_dir_all(&root).expect("root dir");
        std::fs::write(app.path().join("secret.txt"), b"secret").expect("secret");
        let source = Arc::new(
            super::super::Source::prepare(super::super::super::AssetInput::Disk {
                application_dir: app.path().to_owned(),
                selected_root: root,
            })
            .expect("prepared source"),
        );
        let site = Arc::new(Site {
            host: "org2eexample2eapp.alef".into(),
            origin: "https://org2eexample2eapp.alef".into(),
            allowed: Vec::new(),
            assets: source,
            csp: DEFAULT_ORIGIN_CSP.into(),
        });
        for raw in [
            "https://org2eexample2eapp.alef/../secret.txt",
            "https://org2eexample2eapp.alef/%2e%2e/secret.txt",
        ] {
            let url = Url::parse(raw).expect("url");
            match bounded(read(&site, &Method::GET, url.path())).await {
                Ok(asset) => panic!(
                    "{raw} -> {}: unexpectedly served {} bytes",
                    url.path(),
                    asset.bytes.len()
                ),
                Err(404 | 403) => {}
                Err(status) => panic!("{raw} -> {}: HTTP {status}", url.path()),
            }
        }
    }

    // The body has been handed to the caller and the permit is STILL held: only dropping the
    // body releases the admission slot.
    #[tokio::test]
    async fn an_archive_permit_is_still_held_after_the_read_hands_its_bytes_over() {
        let source = Arc::new(
            super::super::Source::prepare(super::super::archive_tests::input("dist"))
                .expect("prepared archive"),
        );
        let site = Arc::new(Site {
            host: "org2eexample2eapp.alef".into(),
            origin: "https://org2eexample2eapp.alef".into(),
            allowed: Vec::new(),
            assets: source,
            csp: DEFAULT_ORIGIN_CSP.into(),
        });
        let asset = bounded(read(&site, &Method::GET, "/"))
            .await
            .expect("index");
        let super::super::Source::Archive(archive) = site.assets.as_ref() else {
            panic!("the fixture must be an archive");
        };
        assert_eq!(
            archive.free_admissions(),
            super::super::archive::RUNNING - 1
        );
        drop(asset);
        assert_eq!(archive.free_admissions(), super::super::archive::RUNNING);
    }

    #[test]
    fn non_get_methods_are_rejected_with_405() {
        for method in [Method::POST, Method::HEAD, Method::OPTIONS, Method::DELETE] {
            assert_eq!(decide(&method), Decision::Reject(405), "{method}");
        }
        assert_eq!(decide(&Method::GET), Decision::Serve);
    }

    #[test]
    fn status_maps_match_the_native_protocol_handler() {
        let status_of = |kind: io::ErrorKind| status_for(&io::Error::from(kind));
        assert_eq!(status_of(io::ErrorKind::PermissionDenied), 403);
        assert_eq!(status_of(io::ErrorKind::NotFound), 404);
        assert_eq!(status_of(io::ErrorKind::InvalidInput), 400);
        assert_eq!(status_of(io::ErrorKind::InvalidData), 400);
        assert_eq!(status_of(io::ErrorKind::WouldBlock), 429);
        assert_eq!(status_of(io::ErrorKind::Other), 500);
    }

    #[test]
    fn error_head_carries_the_native_error_headers_and_status() {
        let head = error_head(404, DEFAULT_ORIGIN_CSP);
        assert_eq!(head.status, StatusCode::NOT_FOUND);
        assert_eq!(head.headers[CONTENT_TYPE], "application/json");
        assert_eq!(head.headers[X_CONTENT_TYPE_OPTIONS], "nosniff");
        assert_eq!(head.headers[CONTENT_SECURITY_POLICY], DEFAULT_ORIGIN_CSP);
        let parsed: serde_json::Value = serde_json::from_slice(&error_body()).expect("json body");
        assert!(parsed
            .get("error")
            .and_then(|value| value.as_str())
            .is_some());
    }

    /// A recording channel: captures start/body/finish; the trait has no cancel operation.
    struct RecordingChannel {
        head: Option<ResponseHead>,
        body: Vec<u8>,
        finished: bool,
        on_finish: Option<Box<dyn FnOnce()>>,
    }

    impl ResponseChannel for RecordingChannel {
        fn start(&mut self, head: ResponseHead) {
            assert!(self.head.is_none(), "start twice");
            self.head = Some(head);
        }
        fn body(&mut self, chunk: Vec<u8>) {
            assert!(self.head.is_some(), "body before start");
            self.body.extend_from_slice(&chunk);
        }
        fn finish(&mut self) {
            assert!(self.head.is_some(), "finish before start");
            if let Some(on_finish) = self.on_finish.take() {
                on_finish();
            }
            self.finished = true;
        }
    }

    #[test]
    fn the_response_channel_starts_streams_the_body_and_finishes_without_cancelling() {
        let mut channel = RecordingChannel {
            head: None,
            body: Vec::new(),
            finished: false,
            on_finish: None,
        };
        respond(
            &mut channel,
            asset_head("text/javascript", DEFAULT_ORIGIN_CSP),
            b"abcdef".to_vec(),
            None,
        );
        let head = channel.head.expect("start was not seen");
        assert_eq!(head.status, StatusCode::OK);
        assert_eq!(head.headers[CONTENT_TYPE], "text/javascript");
        assert_eq!(channel.body, b"abcdef");
        assert!(channel.finished, "finish was not seen");
        // The trait has no cancel operation: a cancel cannot be expressed through `respond`.
    }
    #[test]
    fn an_error_response_goes_through_the_channel_with_status_headers_and_json_body() {
        let mut channel = RecordingChannel {
            head: None,
            body: Vec::new(),
            finished: false,
            on_finish: None,
        };
        respond(
            &mut channel,
            error_head(404, DEFAULT_ORIGIN_CSP),
            error_body(),
            None,
        );
        let head = channel.head.expect("start was not seen");
        assert_eq!(head.status, StatusCode::NOT_FOUND);
        assert_eq!(head.headers[CONTENT_TYPE], "application/json");
        assert_eq!(head.headers[X_CONTENT_TYPE_OPTIONS], "nosniff");
        assert_eq!(head.headers[CONTENT_SECURITY_POLICY], DEFAULT_ORIGIN_CSP);
        assert_eq!(channel.body, error_body());
        assert!(channel.finished, "an error is a complete response");
    }

    #[tokio::test]
    async fn the_permit_is_still_held_when_finish_reports_the_final_handoff() {
        let source = Arc::new(
            super::super::Source::prepare(super::super::archive_tests::input("dist"))
                .expect("prepared archive"),
        );
        let site = Arc::new(Site {
            host: "org2eexample2eapp.alef".into(),
            origin: "https://org2eexample2eapp.alef".into(),
            allowed: Vec::new(),
            assets: source,
            csp: DEFAULT_ORIGIN_CSP.into(),
        });
        let asset = bounded(read(&site, &Method::GET, "/"))
            .await
            .expect("index");
        let super::super::Source::Archive(archive) = site.assets.as_ref() else {
            panic!("the fixture must be an archive");
        };
        let at_finish = std::sync::Arc::new(std::sync::Mutex::new(None::<usize>));
        let observer = {
            let at_finish = at_finish.clone();
            let archive = archive.clone();
            move || {
                *at_finish.lock().expect("lock") = Some(archive.free_admissions());
            }
        };
        let super::AssetBytes {
            bytes,
            mime,
            permit,
        } = asset;
        let mut recording = RecordingChannel {
            head: None,
            body: Vec::new(),
            finished: false,
            on_finish: Some(Box::new(observer)),
        };
        respond(&mut recording, asset_head(&mime, &site.csp), bytes, permit);
        assert_eq!(
            *at_finish.lock().expect("lock"),
            Some(super::super::archive::RUNNING - 1),
            "the permit must still be held when Finish reports the handoff"
        );
        assert_eq!(
            archive.free_admissions(),
            super::super::archive::RUNNING,
            "released only after the handoff function returned"
        );
    }

    #[test]
    fn redact_strips_the_capability_fragment() {
        let url = Url::parse("https://app.alef/#capability=secret-token").expect("url");
        let logged = redact(&url);
        assert!(!logged.contains("secret-token"), "{logged}");
        assert!(!logged.contains('#'), "{logged}");
        assert_eq!(logged, "https://app.alef/");
        let with_query = Url::parse("https://app.alef/big.js?v=1#capability=zz").expect("url");
        assert_eq!(redact(&with_query), "https://app.alef/big.js?v=1");
    }

    // No-site is decided before classify: `web_resource` returns early when `site_snapshot`
    // is `None`, so `classify` never runs in that case (asserted by code reading above).
    #[test]
    fn unknown_hosts_are_cancelled_not_served() {
        let site_origin = "https://app.alef";
        let allowed: Vec<String> = vec!["http://127.0.0.1:8000".into()];
        assert_eq!(
            classify(
                site_origin,
                &allowed,
                "https://evil.example",
                "https",
                Some("evil.example")
            ),
            Classification::Cancelled
        );
        assert_eq!(
            classify(
                site_origin,
                &allowed,
                "http://evil.example",
                "http",
                Some("evil.example")
            ),
            Classification::Cancelled
        );
        // The exact origin of the app host serves the application...
        assert_eq!(
            classify(
                site_origin,
                &allowed,
                site_origin,
                "https",
                Some("app.alef")
            ),
            Classification::App
        );
        // ...but a DIFFERENT PORT of the app host is a different origin and is cancelled.
        assert_eq!(
            classify(
                site_origin,
                &allowed,
                "https://app.alef:8443",
                "https",
                Some("app.alef")
            ),
            Classification::Cancelled
        );
        // Plain http never serves the app origin.
        assert_eq!(
            classify(
                site_origin,
                &allowed,
                "http://app.alef",
                "http",
                Some("app.alef")
            ),
            Classification::Cancelled
        );
        assert_eq!(
            classify(site_origin, &allowed, "native://app", "native", Some("app")),
            Classification::Untouched
        );
        assert_eq!(
            classify(site_origin, &allowed, "https://app.alef", "https", None),
            Classification::Untouched
        );
    }

    // The manifest `external` allow-list exempts origins from the pre-network cancel;
    // everything else on an external host is still refused before the network.
    #[test]
    fn a_listed_external_origin_is_untouched_but_others_on_its_host_are_cancelled() {
        let site_origin = "https://app.alef";
        let allowed: Vec<String> = vec!["http://example.localhost:1".into()];
        assert_eq!(
            classify(
                site_origin,
                &allowed,
                "http://example.localhost:1",
                "http",
                Some("example.localhost")
            ),
            Classification::Untouched
        );
        // Case-insensitive match: an uppercase SOURCE is normalized by `origin_matches`.
        assert_eq!(
            classify(
                site_origin,
                &["HTTP://EXAMPLE.LOCALHOST:1".to_owned()],
                "http://example.localhost:1",
                "http",
                Some("example.localhost")
            ),
            Classification::Untouched
        );
        // A different port of the same host is a different origin and stays cancelled.
        assert_eq!(
            classify(
                site_origin,
                &allowed,
                "http://example.localhost:2",
                "http",
                Some("example.localhost")
            ),
            Classification::Cancelled
        );
    }

    // Wildcard allow-list entries are CSP sources: `*.example.com` covers subdomains of any
    // depth with an exact scheme and port, never the domain itself.
    #[test]
    fn a_wildcard_allow_list_entry_covers_subdomains_only() {
        let site_origin = "https://app.alef";
        let allowed: Vec<String> = vec!["https://*.example.com".into()];
        assert_eq!(
            classify(
                site_origin,
                &allowed,
                "https://a.b.example.com",
                "https",
                Some("a.b.example.com")
            ),
            Classification::Untouched
        );
        for (origin, scheme, host) in [
            ("https://example.com", "https", "example.com"),
            ("https://a.example.com:8443", "https", "a.example.com"),
            ("http://a.example.com", "http", "a.example.com"),
        ] {
            assert_eq!(
                classify(site_origin, &allowed, origin, scheme, Some(host)),
                Classification::Cancelled,
                "{origin}"
            );
        }
    }

    // An older SiteGuard must only clear the site it owns, never the newer installed one.
    #[tokio::test]
    async fn dropping_an_older_site_guard_leaves_the_current_site_installed() {
        let _guard = SITE_TEST_LOCK.lock().await;
        let (_dir, source) = disk_source();
        let older = install(
            "old.org2eexample2eapp.alef".into(),
            source.clone(),
            None,
            Vec::new(),
        );
        let current = install("org2eexample2eapp.alef".into(), source, None, Vec::new());
        drop(older);
        assert_eq!(
            test_site_host().as_deref(),
            Some("org2eexample2eapp.alef"),
            "the older guard must not clear the newer site"
        );
        drop(current);
        assert_eq!(
            test_site_host(),
            None,
            "dropping the current guard must clear the site"
        );
    }

    // Every Servo builder must share one option set: per-app profile root, temporary
    // storage, single process.
    #[test]
    fn servo_options_carry_the_profile_dir_and_temporary_storage() {
        let opts = servo_opts(Some(std::path::PathBuf::from("profile-test")));
        assert_eq!(
            opts.config_dir.as_deref(),
            Some(std::path::Path::new("profile-test"))
        );
        assert!(opts.temporary_storage);
        assert!(!opts.multiprocess);
        assert!(servo_opts(None).config_dir.is_none());
    }
}
