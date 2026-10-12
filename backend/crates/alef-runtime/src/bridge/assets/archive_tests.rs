// SPDX-License-Identifier: MIT OR Apache-2.0
//! Minimal stored ZIP fixtures, independent of the production packer.
use super::*;
use crate::{AssetInput, Bridge, BridgeOptions, Commands};
use alef_package::Package;
use std::io::Cursor;

fn crc(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for byte in bytes {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb8_8320 & 0u32.wrapping_sub(crc & 1));
        }
    }
    !crc
}
fn word(out: &mut Vec<u8>, value: u16) {
    out.extend(value.to_le_bytes());
}
fn dword(out: &mut Vec<u8>, value: u32) {
    out.extend(value.to_le_bytes());
}
pub(super) fn package(entries: &[(&str, &[u8])]) -> Package<Cursor<Vec<u8>>> {
    let mut out = Vec::new();
    let mut central = Vec::new();
    for &(name, bytes) in entries {
        let offset = out.len() as u32;
        let checksum = crc(bytes);
        dword(&mut out, 0x0403_4b50);
        for value in [20, 0x0800, 0, 0, 0x0021] {
            word(&mut out, value);
        }
        for value in [checksum, bytes.len() as u32, bytes.len() as u32] {
            dword(&mut out, value);
        }
        word(&mut out, name.len() as u16);
        word(&mut out, 0);
        out.extend(name.as_bytes());
        out.extend(bytes);

        dword(&mut central, 0x0201_4b50);
        for value in [0x031e, 20, 0x0800, 0, 0, 0x0021] {
            word(&mut central, value);
        }
        for value in [checksum, bytes.len() as u32, bytes.len() as u32] {
            dword(&mut central, value);
        }
        for value in [name.len() as u16, 0, 0, 0, 0] {
            word(&mut central, value);
        }
        dword(&mut central, 0x81a4_0000);
        dword(&mut central, offset);
        central.extend(name.as_bytes());
    }
    let offset = out.len() as u32;
    let size = central.len() as u32;
    out.extend(central);
    dword(&mut out, 0x0605_4b50);
    for value in [0, 0, entries.len() as u16, entries.len() as u16] {
        word(&mut out, value);
    }
    dword(&mut out, size);
    dword(&mut out, offset);
    word(&mut out, 0);
    Package::from_bytes(out).expect("valid unsigned stored Format 1 ZIP")
}
pub(super) fn input(root: &str) -> AssetInput {
    AssetInput::Archive {
        package: package(&[
            ("alef.ktav", b"private manifest"),
            ("icon.png", b"private icon"),
            ("dist2/private.js", b"private"),
            (&format!("{root}/index.html"), b"public"),
            (&format!("{root}/app.js"), b"script"),
            (&format!("{root}/alef.ktav"), b"intentional public manifest"),
        ]),
        selected_root: root.into(),
    }
}

#[test]
fn archive_assets_resolve_exact_roots_and_decode_ascii_once() {
    for root in ["dist", "web/build"] {
        let source = Source::prepare(input(root)).unwrap();
        assert_eq!(source.read("/").unwrap().0, b"public");
        assert_eq!(source.read("/%61pp.js").unwrap().0, b"script");
        assert_eq!(
            source.read("/alef.ktav").unwrap().0,
            b"intentional public manifest"
        );
        let url = url::Url::parse("native://app/app.js?x=../icon.png#fragment").unwrap();
        assert_eq!(source.read(url.path()).unwrap().0, b"script");
        for path in [
            "/APP.JS",
            "/missing",
            "/icon.png",
            "/META-INF/hashes",
            "/dist2/private.js",
        ] {
            assert_eq!(
                source.read(path).unwrap_err().kind(),
                io::ErrorKind::NotFound,
                "{root}: {path}"
            );
        }
    }
}

#[test]
fn archive_roots_require_descendants_and_audit_implicit_siblings() {
    for root in [
        "dist",
        "Dist",
        "dist2",
        "META-INF",
        "meta-inf",
        "",
        "/web",
        "web/..",
        "web//build",
        "web/%61",
        "COM1",
    ] {
        let input = AssetInput::Archive {
            package: package(&[
                ("alef.ktav", b"private"),
                ("dist", b"file"),
                ("web/build/index.html", b"public"),
            ]),
            selected_root: root.into(),
        };
        assert!(Source::prepare(input).is_err(), "{root}");
    }
    let collision = AssetInput::Archive {
        package: package(&[
            ("alef.ktav", b"private"),
            ("dist/A/x", b"x"),
            ("dist/a/y", b"y"),
        ]),
        selected_root: "dist".into(),
    };
    assert_eq!(
        Source::prepare(collision).err().unwrap().kind(),
        io::ErrorKind::PermissionDenied
    );
}

#[test]
fn archive_and_disk_assets_have_identical_mime_types() {
    let (app, root) = super::tests::app("dist");
    let disk = Source::open(app.path(), &root).unwrap();
    let archive = Source::prepare(input("dist")).unwrap();
    for path in ["/", "/app.js", "/%61pp.js"] {
        assert_eq!(disk.read(path).unwrap().1, archive.read(path).unwrap().1);
    }
}

#[tokio::test]
async fn archive_startup_accepts_native_entries_and_refuses_development_urls() {
    let bridge = Bridge::with_options(
        Commands::new(),
        Some(input("web/build")),
        None,
        BridgeOptions {
            entry: Some("/app.js?query=1".into()),
            ..Default::default()
        },
    )
    .await
    .unwrap();
    assert_eq!(bridge.entry_url.path(), "/app.js");
    bridge.shutdown().await.unwrap();
    let missing = Bridge::with_options(
        Commands::new(),
        Some(input("dist")),
        None,
        BridgeOptions {
            entry: Some("/missing".into()),
            ..Default::default()
        },
    )
    .await;
    assert_eq!(missing.err().unwrap().kind(), io::ErrorKind::NotFound);
    let dev = Bridge::with_options(
        Commands::new(),
        Some(input("dist")),
        Some(url::Url::parse("http://localhost:3000/").unwrap()),
        BridgeOptions::default(),
    )
    .await;
    assert_eq!(dev.err().unwrap().kind(), io::ErrorKind::InvalidInput);
}

async fn bounded<T>(future: impl std::future::Future<Output = T>) -> T {
    tokio::time::timeout(std::time::Duration::from_secs(10), future)
        .await
        .expect("operation must complete within ten seconds")
}

fn protocol(source: std::sync::Arc<Source>, csp: Option<&str>) -> super::super::MemoryProtocol {
    use alef_core::protocol::{
        call::Limits,
        transport::{Transport, TransportConfig},
    };
    use alef_core::session::{session::SessionManager, TokenSource};
    let tokens: TokenSource = std::sync::Arc::new(|| "test-token".into());
    let sessions = std::sync::Arc::new(SessionManager::new(tokens, Limits::default()));
    let config = TransportConfig {
        bootstrap_token: "bootstrap".into(),
        allowed_origins: vec![],
        runtime_version: "test".into(),
        limits: Limits::default(),
    };
    let commands = Commands::new();
    super::super::MemoryProtocol {
        assets: Some(source),
        handle: tokio::runtime::Handle::current(),
        transport: std::sync::Arc::new(
            Transport::new(
                config,
                super::super::testing::registry_of(&commands),
                sessions,
                super::super::testing::permissions(),
            )
            .unwrap(),
        ),
        windows: Default::default(),
        limits: Limits::default(),
        csp: csp.map(str::to_owned),
        log_calls: false,
    }
}

fn request(url: &str, method: http::Method) -> servo::protocol_handler::Request {
    use net_traits::request::{Referrer, RequestBuilder};
    RequestBuilder::new(
        None,
        net_traits::blob_url_store::UrlWithBlobClaim::new(
            servo::ServoUrl::parse(url).unwrap(),
            None,
        ),
        Referrer::NoReferrer,
    )
    .method(method)
    .build()
}

#[test]
fn the_independent_zip_builder_produces_a_fully_verified_package() {
    let mut verified = package(&[("alef.ktav", b"manifest"), ("dist/index.html", b"public")]);
    assert!(matches!(verified.signer(), alef_package::Signer::Unsigned));
    assert_eq!(
        verified.names().collect::<Vec<_>>(),
        ["alef.ktav", "dist/index.html"]
    );
    assert_eq!(verified.read("dist/index.html").unwrap(), b"public");
}

#[test]
fn archive_reads_accept_the_inclusive_limit_and_reject_declared_excess_before_decoding() {
    let mut source = Source::prepare(input("dist")).unwrap();
    let Source::Archive(archive) = &mut source else {
        unreachable!()
    };
    std::sync::Arc::get_mut(archive).unwrap().set_limit(6);
    assert_eq!(source.read("/").unwrap().0, b"public");
    let Source::Archive(archive) = &mut source else {
        unreachable!()
    };
    std::sync::Arc::get_mut(archive).unwrap().set_limit(5);
    let error = source.read("/").unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    let error = error
        .get_ref()
        .unwrap()
        .downcast_ref::<alef_package::PackageError>()
        .unwrap();
    assert!(
        matches!(error, alef_package::PackageError::Entry(name, inner) if name == "dist/index.html" && matches!(inner.as_ref(), alef_package::PackageError::ReadSizeLimit(5)))
    );
}

#[tokio::test]
async fn memory_protocol_assets_enforce_get_host_csp_nosniff_and_exact_mime() {
    for csp in [None, Some("default-src 'none'")] {
        let source = std::sync::Arc::new(Source::prepare(input("dist")).unwrap());
        let protocol = protocol(source, csp);
        for (url, method, status) in [
            (
                "native://app/%61pp.js?ignored=../icon.png#ignored",
                http::Method::GET,
                200,
            ),
            ("native://app/app.js", http::Method::POST, 404),
            ("native://other/app.js", http::Method::GET, 404),
            ("native://app/missing", http::Method::GET, 404),
            ("native://app/a%2fb", http::Method::GET, 400),
        ] {
            let response = bounded(protocol.asset_response(&request(url, method))).await;
            assert_eq!(response.status.raw_code(), status);
            assert_eq!(
                response.headers[http::header::X_CONTENT_TYPE_OPTIONS],
                "nosniff"
            );
            let expected = if url.starts_with("native://app/") {
                Some(csp.unwrap_or(super::super::DEFAULT_CSP))
            } else {
                None
            };
            assert_eq!(
                response
                    .headers
                    .get(http::header::CONTENT_SECURITY_POLICY)
                    .map(|h| h.to_str().unwrap()),
                expected
            );
            assert!(!response
                .headers
                .contains_key(http::header::ACCESS_CONTROL_ALLOW_ORIGIN));
            if status == 200 {
                assert_eq!(
                    response.headers[http::header::CONTENT_TYPE],
                    mime_guess::from_path("app.js")
                        .first_or_octet_stream()
                        .to_string()
                );
                assert!(
                    matches!(&*response.body.lock(), servo::protocol_handler::ResponseBody::Done(bytes) if bytes == b"script")
                );
            }
        }
    }
}

#[tokio::test]
async fn four_queued_archive_jobs_reject_the_fifth_and_cancellation_keeps_admission() {
    let source = std::sync::Arc::new(Source::prepare(input("dist")).unwrap());
    let Source::Archive(archive) = source.as_ref() else {
        unreachable!()
    };
    // The guard lives on a blocking worker, never across an await of the test;
    // all synchronization waits are bounded.
    let archive = std::sync::Arc::clone(archive);
    let (locked, release) = archive.hold_package();
    // Bounded wait until the blocking worker actually holds the package mutex.
    let _ = bounded(tokio::task::spawn_blocking(move || locked.recv().unwrap())).await;
    let mut jobs = Vec::new();
    for _ in 0..4 {
        let source = source.clone();
        jobs.push(tokio::spawn(async move {
            read_asset(Some(source), "/".into()).await
        }));
        bounded(archive.admitted()).await;
    }
    assert_eq!(
        bounded(read_asset(Some(source.clone()), "/".into()))
            .await
            .unwrap_err()
            .kind(),
        io::ErrorKind::WouldBlock
    );
    let handler = protocol(source.clone(), None);
    assert_eq!(
        bounded(handler.asset_response(&request("native://app/", http::Method::GET)))
            .await
            .status
            .raw_code(),
        429
    );
    let cancelled = jobs.pop().unwrap();
    cancelled.abort();
    assert!(bounded(cancelled).await.unwrap_err().is_cancelled());
    assert_eq!(
        bounded(read_asset(Some(source.clone()), "/".into()))
            .await
            .unwrap_err()
            .kind(),
        io::ErrorKind::WouldBlock
    );
    drop(release);
    for job in jobs {
        drop(bounded(job).await.unwrap().unwrap());
    }
    bounded(archive.drained()).await;
    assert_eq!(
        bounded(read_asset(Some(source), "/".into()))
            .await
            .unwrap()
            .bytes,
        b"public"
    );
}

#[tokio::test]
async fn completed_archive_bytes_keep_admission_until_the_response_handoff() {
    let source = std::sync::Arc::new(Source::prepare(input("dist")).unwrap());
    let mut bytes = Vec::new();
    for _ in 0..4 {
        bytes.push(
            bounded(read_asset(Some(source.clone()), "/".into()))
                .await
                .unwrap(),
        );
    }
    assert_eq!(
        bounded(read_asset(Some(source.clone()), "/".into()))
            .await
            .unwrap_err()
            .kind(),
        io::ErrorKind::WouldBlock
    );
    drop(bytes.pop());
    let handler = protocol(source.clone(), None);
    assert_eq!(
        bounded(handler.asset_response(&request("native://app/", http::Method::GET)))
            .await
            .status
            .raw_code(),
        200
    );
    // The bridge hands bytes to Servo and releases its permit, not renderer memory.
    assert_eq!(
        bounded(read_asset(Some(source), "/".into()))
            .await
            .unwrap()
            .bytes,
        b"public"
    );
}

#[tokio::test]
async fn missing_entry_or_initial_window_documents_fail_before_module_installation() {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };
    for (entry, documents, kind) in [
        ("/missing", vec![], io::ErrorKind::NotFound),
        ("/", vec!["/missing"], io::ErrorKind::NotFound),
        (
            "/",
            vec!["native://other/app.js"],
            io::ErrorKind::InvalidInput,
        ),
        ("/", vec!["/a%2fb"], io::ErrorKind::InvalidInput),
    ] {
        let called = Arc::new(AtomicBool::new(false));
        let callback = called.clone();
        let options = BridgeOptions {
            entry: Some(entry.into()),
            initial_document_urls: documents.into_iter().map(str::to_owned).collect(),
            modules: Some(Box::new(move |_, _| {
                callback.store(true, Ordering::SeqCst);
                Ok(())
            })),
            ..Default::default()
        };
        let result = bounded(Bridge::with_options(
            Commands::new(),
            Some(input("dist")),
            None,
            options,
        ))
        .await;
        assert_eq!(result.err().unwrap().kind(), kind);
        assert!(!called.load(Ordering::SeqCst));
    }
    let bridge = bounded(Bridge::with_options(
        Commands::new(),
        Some(input("dist")),
        None,
        BridgeOptions {
            initial_document_urls: vec!["/%61pp.js?query=1#fragment".into()],
            ..Default::default()
        },
    ))
    .await
    .unwrap();
    bounded(bridge.shutdown()).await.unwrap();
}

#[test]
fn an_encoded_percent_is_decoded_exactly_once() {
    let source = Source::prepare(AssetInput::Archive {
        package: package(&[
            ("alef.ktav", b"private"),
            ("dist/index.html", b"public"),
            ("dist/A.txt", b"decoded"),
        ]),
        selected_root: "dist".into(),
    })
    .unwrap();
    assert_eq!(source.read("/%41.txt").unwrap().0, b"decoded");
    // %25 is a percent sign and no entry can have one in its name: a second decoding would find A.txt.
    assert!(source.read("/%2541.txt").is_err());
}

#[test]
fn the_metadata_folder_is_never_a_public_root_even_when_it_has_entries() {
    // SHA-256 of "abc" in the hash list that the metadata folder of a package really holds.
    let hashes = b"ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad  alef.ktav\n";
    let input = AssetInput::Archive {
        package: package(&[("alef.ktav", b"abc"), ("META-INF/hashes", hashes)]),
        selected_root: "META-INF".into(),
    };
    assert!(Source::prepare(input).is_err());
}
