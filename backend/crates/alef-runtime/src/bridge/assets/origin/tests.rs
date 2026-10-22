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
    let super::super::AssetBytes {
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

// With temporary_storage the HTTP disk cache must be disabled: Servo would otherwise put a
// shared cache.sqlite3 in an OS tempdir, outside the per-app profile (origin/tests.rs style note).
#[test]
fn preferences_disable_the_disk_http_cache_under_temporary_storage() {
    assert!(servo_opts(None).temporary_storage);
    assert!(!preferences_for(None).network_http_disk_cache.is_empty());
}
