// SPDX-License-Identifier: MIT OR Apache-2.0
// M0.1: application origin via load_web_resource (docs/stages/m0-spikes.md).
use std::io;
use std::path::{Path, PathBuf};
use std::sync::OnceLock;
use std::time::Instant;

use embedder_traits::WebResourceResponse;
use http::header::{CONTENT_SECURITY_POLICY, CONTENT_TYPE, X_CONTENT_TYPE_OPTIONS};
use http::{HeaderMap, HeaderValue, Method, StatusCode};
use servo::{Preferences, WebResourceLoad};
use url::Url;

const HOST: &str = "spike-app.alef";
const CHUNK_BYTES: usize = 256 * 1024;
// example.com is allowed here only so the interception-level block (cancel) is
// exercised instead of being short-circuited by CSP; production CSP is stricter.
const CSP: &str = "default-src 'none'; script-src 'self'; style-src 'self' 'unsafe-inline'; img-src 'self' data:; connect-src 'self' native: https://example.com; base-uri 'none'; object-src 'none'; frame-src 'none'; form-action 'none'";

static ASSETS: OnceLock<PathBuf> = OnceLock::new();

enum AssetMessage {
    Chunk(Vec<u8>),
    Done(u64),
    Failed(String),
}

pub(crate) fn enabled() -> bool {
    std::env::var("ALEF_SPIKE_ORIGIN").is_ok_and(|value| value == "1")
}

pub(crate) fn entry_url() -> Option<Url> {
    enabled().then(app_origin_url)
}

/// Only flips `dom_indexeddb_enabled`; identical to the builder default otherwise.
pub(crate) fn preferences() -> Preferences {
    Preferences {
        dom_indexeddb_enabled: enabled(),
        ..Default::default()
    }
}

pub(crate) fn set_assets(assets: &Path) {
    let _ = ASSETS.set(assets.to_path_buf());
}

fn app_origin_url() -> Url {
    Url::parse(&format!("https://{HOST}/")).expect("constant URL")
}

/// URL for logs without the fragment: the entry fragment carries the bridge capability.
fn redact(url: &Url) -> String {
    let mut clean = url.clone();
    clean.set_fragment(None);
    clean.to_string()
}

/// Response decision for a request to the app origin; pure so unit tests need no Servo types.
#[derive(Debug, PartialEq)]
enum Decision {
    Serve { path: PathBuf, mime: String },
    Reject(StatusCode),
}

fn decide(method: &Method, url: &Url, root: &Path) -> Decision {
    if *method != Method::GET {
        return Decision::Reject(StatusCode::METHOD_NOT_ALLOWED);
    }
    match resolve_asset(root, url.path()) {
        Ok(path) => Decision::Serve {
            mime: mime_guess::from_path(&path)
                .first_or_octet_stream()
                .to_string(),
            path,
        },
        Err(error) => Decision::Reject(status_for(&error)),
    }
}

fn status_for(error: &io::Error) -> StatusCode {
    if error.kind() == io::ErrorKind::PermissionDenied {
        StatusCode::FORBIDDEN
    } else {
        StatusCode::NOT_FOUND
    }
}

/// Runs on the main thread: Servo dispatches `NetToEmbedderMsg` from `spin_event_loop`.
/// `WebResourceLoad`/`InterceptedWebResourceLoad` are not `Send` (Box<dyn AbstractSender>),
/// so interception is completed here; only the disk read runs on Tokio.
pub(crate) fn web_resource(load: WebResourceLoad) {
    if !enabled() {
        return; // Dropping the load means DoNotIntercept.
    }
    let request = load.request.clone();
    let url = request.url.clone();
    eprintln!(
        "origin-spike: {} {} main_frame={} destination={:?} thread={}",
        request.method,
        redact(&url),
        request.is_for_main_frame,
        request.destination,
        std::thread::current().name().unwrap_or("<unnamed>"),
    );
    let scheme = url.scheme();
    if (scheme == "http" || scheme == "https") && url.host_str() != Some(HOST) {
        let started = Instant::now();
        // cancel() lives on InterceptedWebResourceLoad; intercept first, then cancel.
        load.intercept(WebResourceResponse::new(url)).cancel();
        eprintln!(
            "origin-spike: external request cancelled in {:?} (interception precedes scheme fetch)",
            started.elapsed()
        );
        return;
    }
    if scheme != "https" || url.host_str() != Some(HOST) {
        return; // native:, about:, blob: etc. keep their normal path.
    }
    let Some(root) = ASSETS.get() else {
        reject(load, url, StatusCode::NOT_FOUND);
        return;
    };
    match decide(&request.method, &url, root) {
        Decision::Serve { path, mime } => serve(load, url, path, mime),
        Decision::Reject(status) => {
            eprintln!("origin-spike: rejected {} {}", status, redact(&url));
            reject(load, url, status);
        }
    }
}

fn reject(load: WebResourceLoad, url: Url, status: StatusCode) {
    let response = WebResourceResponse::new(url)
        .status_code(status)
        .status_message(b"Rejected by origin spike".to_vec());
    load.intercept(response).finish();
}

fn serve(load: WebResourceLoad, url: Url, path: PathBuf, mime: String) {
    let (sender, receiver) = std::sync::mpsc::channel();
    let task_path = path.clone();
    tokio::runtime::Handle::current().spawn(async move {
        read_asset(task_path, sender).await;
    });
    let started = Instant::now();
    let mut intercepted = load.intercept(asset_response(url, &mime));
    loop {
        match receiver.recv() {
            Ok(AssetMessage::Chunk(chunk)) => intercepted.send_body_data(chunk),
            Ok(AssetMessage::Done(total)) => {
                intercepted.finish();
                eprintln!(
                    "origin-spike: served {} {total} B in {:?}",
                    path.display(),
                    started.elapsed()
                );
                return;
            }
            Ok(AssetMessage::Failed(error)) => {
                eprintln!("origin-spike: read failed for {}: {error}", path.display());
                intercepted.cancel();
                return;
            }
            Err(_) => {
                eprintln!("origin-spike: reader task vanished for {}", path.display());
                return; // Drop finishes the interception.
            }
        }
    }
}

async fn read_asset(path: PathBuf, sender: std::sync::mpsc::Sender<AssetMessage>) {
    match tokio::fs::read(&path).await {
        Ok(bytes) => {
            let total = bytes.len() as u64;
            for chunk in bytes.chunks(CHUNK_BYTES) {
                if sender.send(AssetMessage::Chunk(chunk.to_vec())).is_err() {
                    return;
                }
            }
            let _ = sender.send(AssetMessage::Done(total));
        }
        Err(error) => {
            let _ = sender.send(AssetMessage::Failed(error.to_string()));
        }
    }
}

fn asset_response(url: Url, mime: &str) -> WebResourceResponse {
    let mut headers = HeaderMap::new();
    headers.insert(
        CONTENT_TYPE,
        HeaderValue::from_str(mime).expect("MIME header"),
    );
    headers.insert(CONTENT_SECURITY_POLICY, HeaderValue::from_static(CSP));
    headers.insert(X_CONTENT_TYPE_OPTIONS, HeaderValue::from_static("nosniff"));
    WebResourceResponse::new(url).headers(headers)
}

/// "/" maps to index.html; no traversal; result stays under the canonical root.
fn resolve_asset(root: &Path, path: &str) -> io::Result<PathBuf> {
    let requested = path.trim_start_matches('/');
    let requested = if requested.is_empty() {
        "index.html"
    } else {
        requested
    };
    if requested.split(['/', '\\']).any(|segment| segment == "..") {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Asset outside frontend root",
        ));
    }
    let canonical = root.join(requested).canonicalize()?;
    if !canonical.starts_with(root) {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "Asset outside frontend root",
        ));
    }
    Ok(canonical)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn app_origin_is_the_spike_https_host() {
        let url = app_origin_url();
        assert_eq!(url.scheme(), "https");
        assert_eq!(url.host_str(), Some(HOST));
        assert_eq!(url.path(), "/");
        assert_eq!(
            url.origin().unicode_serialization(),
            "https://spike-app.alef"
        );
    }

    #[test]
    fn redact_strips_the_capability_fragment() {
        let url = Url::parse("https://spike-app.alef/#capability=secret-token").expect("url");
        let logged = redact(&url);
        assert!(!logged.contains("secret-token"), "{logged}");
        assert!(!logged.contains('#'), "{logged}");
        assert_eq!(logged, "https://spike-app.alef/");
        let with_query =
            Url::parse("https://spike-app.alef/big.js?v=1#capability=zz").expect("url");
        assert_eq!(redact(&with_query), "https://spike-app.alef/big.js?v=1");
    }

    #[test]
    fn decide_serves_existing_file_with_mime() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        std::fs::write(temporary.path().join("app.js"), b"1").expect("file");
        let root = temporary.path().canonicalize().expect("root");
        let url = Url::parse("https://spike-app.alef/app.js").expect("url");
        assert_eq!(
            decide(&Method::GET, &url, &root),
            Decision::Serve {
                path: root.join("app.js"),
                mime: "text/javascript".to_owned(),
            }
        );
    }

    #[test]
    fn decide_rejects_missing_and_non_get() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = temporary.path().canonicalize().expect("root");
        let url = Url::parse("https://spike-app.alef/absent.js").expect("url");
        assert_eq!(
            decide(&Method::GET, &url, &root),
            Decision::Reject(StatusCode::NOT_FOUND)
        );
        assert_eq!(
            decide(&Method::POST, &url, &root),
            Decision::Reject(StatusCode::METHOD_NOT_ALLOWED)
        );
    }

    #[test]
    fn status_for_maps_traversal_and_missing_kinds() {
        assert_eq!(
            status_for(&io::Error::from(io::ErrorKind::PermissionDenied)),
            StatusCode::FORBIDDEN
        );
        assert_eq!(
            status_for(&io::Error::from(io::ErrorKind::NotFound)),
            StatusCode::NOT_FOUND
        );
    }

    #[test]
    fn asset_response_is_ok_with_csp_and_content_type() {
        let response = asset_response(app_origin_url(), "text/javascript");
        assert_eq!(response.status_code, StatusCode::OK);
        assert_eq!(
            response.headers.get(CONTENT_TYPE),
            Some(&HeaderValue::from_static("text/javascript"))
        );
        assert!(response.headers.contains_key(CONTENT_SECURITY_POLICY));
        assert!(response.headers.contains_key(X_CONTENT_TYPE_OPTIONS));
    }

    #[test]
    fn empty_path_maps_to_index_html() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        std::fs::write(temporary.path().join("index.html"), b"<html>").expect("file");
        let root = temporary.path().canonicalize().expect("root");
        let resolved = resolve_asset(&root, "/").expect("index");
        assert_eq!(resolved, root.join("index.html"));
    }

    #[test]
    fn traversal_is_rejected() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = temporary.path().canonicalize().expect("root");
        for path in ["/../secret", "/a/../../secret", "/..\\secret"] {
            let error = resolve_asset(&root, path).expect_err("traversal denied");
            assert_eq!(error.kind(), io::ErrorKind::PermissionDenied, "{path}");
        }
    }

    // The URL parser normalizes dot segments, so a parsed URL can never carry `..`
    // into the resolver: every escape attempt ends as a plain 404 inside the root.
    #[test]
    fn decide_never_serves_outside_the_root_for_dot_dot_urls() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let outer = temporary.path().canonicalize().expect("outer");
        std::fs::create_dir(outer.join("root")).expect("root dir");
        std::fs::write(outer.join("secret.txt"), b"secret").expect("secret");
        let root = outer.join("root").canonicalize().expect("root");
        for raw in [
            "https://spike-app.alef/../secret.txt",
            "https://spike-app.alef/a/../../secret.txt",
            "https://spike-app.alef/%2e%2e/secret.txt",
            "https://spike-app.alef/..\\secret.txt",
        ] {
            let url = Url::parse(raw).expect("url");
            assert_eq!(
                decide(&Method::GET, &url, &root),
                Decision::Reject(StatusCode::NOT_FOUND),
                "{raw} -> {}",
                url.path()
            );
        }
    }

    #[test]
    fn missing_file_is_not_found() {
        let temporary = tempfile::tempdir().expect("temporary directory");
        let root = temporary.path().canonicalize().expect("root");
        let error = resolve_asset(&root, "/absent.js").expect_err("missing");
        assert!(matches!(
            error.kind(),
            io::ErrorKind::NotFound | io::ErrorKind::InvalidInput
        ));
    }
}
