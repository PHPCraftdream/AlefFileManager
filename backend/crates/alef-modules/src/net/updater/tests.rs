// SPDX-License-Identifier: MIT OR Apache-2.0
//! Loopback integration tests for updater feed checks.
use super::*;
use alef_core::{
    protocol::call::Limits,
    registry::{command::Reply, context::CallContext, dispatch::Registry},
    security::{
        manifest::Permissions,
        permissions::{PathVars, PermissionSet},
    },
    session::{session::SessionManager, Session, TokenSource},
};
use base64::{engine::general_purpose::STANDARD, Engine as _};
use http_body_util::BodyExt as _;
use hyper::{body::Incoming, service::service_fn, Request, Response};
use hyper_util::rt::TokioIo;
use ring::{
    digest,
    signature::{Ed25519KeyPair, KeyPair},
};
use std::{convert::Infallible, net::SocketAddr, sync::Arc, time::Duration};
use tokio::{
    net::TcpListener,
    task::{JoinHandle, JoinSet},
};

const WAIT: Duration = Duration::from_secs(5);

struct Server {
    url: String,
    task: JoinHandle<()>,
}
impl Drop for Server {
    fn drop(&mut self) {
        self.task.abort();
    }
}
impl Server {
    async fn start(body: Vec<u8>, status: hyper::StatusCode, redirect: bool) -> Self {
        Self::start_with_length(body, status, redirect, None).await
    }
    async fn start_with_length(
        body: Vec<u8>,
        status: hyper::StatusCode,
        redirect: bool,
        content_length: Option<String>,
    ) -> Self {
        Self::start_with_body(body, status, redirect, content_length, false).await
    }
    async fn start_with_body(
        body: Vec<u8>,
        status: hyper::StatusCode,
        redirect: bool,
        content_length: Option<String>,
        suppress_length: bool,
    ) -> Self {
        let listener = tokio::time::timeout(WAIT, TcpListener::bind("127.0.0.1:0"))
            .await
            .unwrap()
            .unwrap();
        let address: SocketAddr = listener.local_addr().unwrap();
        let task = tokio::spawn(async move {
            let mut connections = JoinSet::new();
            loop {
                tokio::select! {
                    accepted = listener.accept() => {
                        let Ok((socket, _)) = accepted else { break };
                        let body = body.clone();
                        let content_length = content_length.clone();
                        connections.spawn(async move {
                            let service = service_fn(move |_request: Request<Incoming>| {
                                let body = body.clone();
                                let content_length = content_length.clone();
                                async move {
                                    let mut response = if suppress_length {
                                        Response::new(http_body_util::StreamBody::new(
                                            futures_util::stream::iter([Ok::<_, Infallible>(
                                                hyper::body::Frame::data(bytes::Bytes::from(body)),
                                            )]),
                                        ).boxed())
                                    } else {
                                        Response::new(http_body_util::Full::new(bytes::Bytes::from(body)).boxed())
                                    };
                                    *response.status_mut() = status;
                                    if redirect {
                                        response.headers_mut().insert(
                                            hyper::header::LOCATION,
                                            hyper::header::HeaderValue::from_static("/elsewhere"),
                                        );
                                    }
                                    if let Some(value) = content_length {
                                        response.headers_mut().insert(
                                            hyper::header::CONTENT_LENGTH,
                                            hyper::header::HeaderValue::from_str(&value).unwrap(),
                                        );
                                    }
                                    Ok::<_, Infallible>(response)
                                }
                            });
                            let _ = hyper::server::conn::http1::Builder::new()
                                .serve_connection(TokioIo::new(socket), service)
                                .await;
                        });
                    }
                    _ = connections.join_next(), if !connections.is_empty() => {}
                }
            }
            connections.abort_all();
        });
        Self {
            url: format!("http://{address}/feed"),
            task,
        }
    }
}

fn permissions() -> Arc<PermissionSet> {
    let policy: Permissions = serde_json::from_value(serde_json::json!({"fs":{"read":[],"write":[]},"cli":{"exec":[]},"net":{"http":[],"socket":[]},"shell":{"openExternal":[]},"clipboard":{"read":false},"shortcut":{"global":false},"secrets":false,"app":{"env":[]}})).unwrap();
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
    Arc::new(PermissionSet::from_manifest(&policy, &vars).unwrap())
}
async fn session() -> Arc<Session> {
    let tokens: TokenSource = Arc::new(|| "updater-test-token".to_owned());
    tokio::time::timeout(WAIT, async {
        SessionManager::new(tokens, Limits::default())
            .begin_document(1)
            .await
    })
    .await
    .unwrap()
}
async fn check(endpoint: String) -> Result<Reply, AlefError> {
    let ctx = UpdaterContext {
        config: Updater {
            endpoint,
            public_key: STANDARD.encode([7u8; 32]),
        },
        app_id: "test.app".into(),
        version: "1.0.0".into(),
        trusted_key: [7u8; 32],
        data: std::env::temp_dir().join("alef-updater-tests"),
    };
    let mut registry = Registry::default();
    register_updater(&mut registry, &Some(ctx)).unwrap();
    tokio::time::timeout(
        WAIT,
        registry.dispatch(
            "updater.check",
            CallContext::new(session().await, permissions()),
            serde_json::json!({}),
        ),
    )
    .await
    .unwrap()
}
fn document(version: &str, package: &str) -> Vec<u8> {
    serde_json::json!({"version":version,"package":package,"sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}).to_string().into_bytes()
}

#[tokio::test]
async fn check_returns_unavailable_for_versions_that_are_not_newer() {
    for version in ["1.0.0", "0.9.0"] {
        let server =
            Server::start(document(version, "/package"), hyper::StatusCode::OK, false).await;
        let reply = check(server.url.clone()).await.unwrap();
        assert!(
            matches!(reply, Reply::Json(value) if value == serde_json::json!({"available":false}))
        );
    }
}

#[tokio::test]
async fn check_returns_an_available_newer_update() {
    let server = Server::start(document("1.2.0", "/package"), hyper::StatusCode::OK, false).await;
    let reply = check(server.url.clone()).await.unwrap();
    assert!(
        matches!(reply, Reply::Json(value) if value == serde_json::json!({"available":true,"version":"1.2.0"}))
    );
}

#[tokio::test]
async fn check_rejects_malformed_oversized_and_redirected_documents() {
    let cases = [
        (
            b"{".to_vec(),
            hyper::StatusCode::OK,
            ErrorCode::UpdateBadDocument,
        ),
        (
            vec![b'x'; document::MAX_DOCUMENT + 1],
            hyper::StatusCode::OK,
            ErrorCode::UpdateTooLarge,
        ),
        (
            document("2.0.0", "/package"),
            hyper::StatusCode::FOUND,
            ErrorCode::UpdateNetwork,
        ),
    ];
    for (body, status, expected) in cases {
        let server = Server::start(body, status, status.is_redirection()).await;
        let error = check(server.url.clone()).await.unwrap_err();
        assert_eq!(error.code, expected);
    }
}

#[tokio::test]
async fn transfer_rejects_hash_mismatch_and_removes_temporary_package() {
    let server = Server::start(b"wrong digest".to_vec(), hyper::StatusCode::OK, false).await;
    assert_transfer_error(
        &server,
        "0".repeat(64),
        download::MAX_PACKAGE_BYTES,
        ErrorCode::UpdateHashMismatch,
    )
    .await;
}

#[tokio::test]
async fn transfer_rejects_oversized_content_length_before_creating_temp_package() {
    let server = Server::start_with_body(
        vec![],
        hyper::StatusCode::OK,
        false,
        Some((download::MAX_PACKAGE_BYTES + 1).to_string()),
        true,
    )
    .await;
    assert_transfer_error(
        &server,
        "0".repeat(64),
        download::MAX_PACKAGE_BYTES,
        ErrorCode::UpdateTooLarge,
    )
    .await;
}

#[tokio::test]
async fn transfer_rejects_oversized_stream_without_buffering_package() {
    let server =
        Server::start_with_body(vec![b'x'; 64], hyper::StatusCode::OK, false, None, true).await;
    assert_transfer_error(&server, "0".repeat(64), 32, ErrorCode::UpdateTooLarge).await;
}

async fn assert_transfer_error(server: &Server, sha256: String, limit: u64, expected: ErrorCode) {
    let scratch = tempfile::tempdir().unwrap();
    let config = UpdaterContext {
        config: Updater {
            endpoint: server.url.clone(),
            public_key: STANDARD.encode([7u8; 32]),
        },
        app_id: "test.app".into(),
        version: "1.0.0".into(),
        trusted_key: [7u8; 32],
        data: scratch.path().to_path_buf(),
    };
    let offered = document::Document {
        version: "2.0.0".into(),
        notes: None,
        package: server.url.clone(),
        sha256,
    };
    let session = session().await;
    let (writer, _stream) = session.streams().open_outgoing();
    let error = tokio::time::timeout(WAIT, download::transfer(&config, &offered, &writer, limit))
        .await
        .unwrap()
        .unwrap_err();
    assert_eq!(error.code, expected);
    let entries: Vec<_> = std::fs::read_dir(scratch.path().join("updates"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(entries, [std::ffi::OsString::from(".alef-update.lock")]);
}

#[tokio::test]
async fn download_rejects_invalid_packages_without_publishing_artifacts() {
    let scratch = tempfile::tempdir().unwrap();
    let config = UpdaterContext {
        config: Updater {
            endpoint: "http://127.0.0.1/".into(),
            public_key: STANDARD.encode([7u8; 32]),
        },
        app_id: "test.app".into(),
        version: "1.0.0".into(),
        trusted_key: [7u8; 32],
        data: scratch.path().to_path_buf(),
    };
    let storage = storage::Storage::prepare(&config.data).unwrap();
    let mut temp = storage.temp().unwrap();
    std::io::Write::write_all(temp.as_file_mut(), b"not a package").unwrap();
    let offered = document::Document {
        version: "2.0.0".into(),
        notes: None,
        package: "http://127.0.0.1/package".into(),
        sha256: "0".repeat(64),
    };

    let error = tokio::time::timeout(
        WAIT,
        storage::verify_and_publish(storage, temp, &config, &offered),
    )
    .await
    .unwrap()
    .unwrap_err();
    assert_eq!(error.code, ErrorCode::UpdateSignatureInvalid);

    let updates = scratch.path().join("updates");
    let entries: Vec<_> = std::fs::read_dir(&updates)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(entries, [std::ffi::OsString::from(".alef-update.lock")]);
}

#[test]
fn download_storage_lock_prevents_a_second_active_update() {
    let scratch = tempfile::tempdir().unwrap();
    let storage = storage::Storage::prepare(scratch.path()).unwrap();
    let error = match storage::Storage::prepare(scratch.path()) {
        Ok(_) => panic!("second update unexpectedly acquired the lock"),
        Err(error) => error,
    };
    assert_eq!(error.code, ErrorCode::UpdateBusy);
    drop(storage);
    assert!(storage::Storage::prepare(scratch.path()).is_ok());
}

fn signed_package(version: &str, app_id: &str) -> (Vec<u8>, [u8; 32]) {
    let key = Ed25519KeyPair::from_seed_unchecked(&[42u8; 32]).unwrap();
    let manifest = include_bytes!("../../../../alef-core/tests/fixtures/minimal.ktav");
    let manifest = std::str::from_utf8(manifest).unwrap();
    assert!(manifest.contains("id: org.example.app"));
    assert!(manifest.contains("version:: 1.0.0"));
    let manifest = manifest
        .replace("org.example.app", app_id)
        .replace("version:: 1.0.0", &format!("version:: {version}"));
    assert!(manifest.contains(&format!("id: {app_id}")));
    assert!(manifest.contains(&format!("version:: {version}")));
    let payloads: Vec<(&str, Vec<u8>)> = vec![
        ("alef.ktav", manifest.into_bytes()),
        (
            "dist/index.html",
            b"<!doctype html><title>Updater fixture</title>".to_vec(),
        ),
    ];
    let mut hashes = String::new();
    for (name, bytes) in &payloads {
        hashes.push_str(&format!(
            "{}  {name}\n",
            hex(digest::digest(&digest::SHA256, bytes).as_ref())
        ));
    }
    let message = [b"alef-package-v1\n".as_slice(), hashes.as_bytes()].concat();
    let signature = key.sign(&message);
    let signature_record = format!(
        "alef-package 1\nkey {}\nsig {}\n",
        STANDARD.encode(key.public_key().as_ref()),
        STANDARD.encode(signature.as_ref())
    );
    let mut records: Vec<(&str, Vec<u8>)> = payloads;
    records.push(("META-INF/hashes", hashes.into_bytes()));
    records.push(("META-INF/signature", signature_record.into_bytes()));
    (
        stored_zip(records),
        key.public_key().as_ref().try_into().unwrap(),
    )
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}
fn put16(bytes: &mut [u8], offset: usize, value: u16) {
    bytes[offset..offset + 2].copy_from_slice(&value.to_le_bytes());
}
fn put32(bytes: &mut [u8], offset: usize, value: u32) {
    bytes[offset..offset + 4].copy_from_slice(&value.to_le_bytes());
}
fn crc32(bytes: &[u8]) -> u32 {
    let mut crc = !0u32;
    for &byte in bytes {
        crc ^= u32::from(byte);
        for _ in 0..8 {
            crc = (crc >> 1) ^ (0xedb88320 & (0u32.wrapping_sub(crc & 1)));
        }
    }
    !crc
}

fn stored_zip(mut files: Vec<(&str, Vec<u8>)>) -> Vec<u8> {
    files.sort_by_key(|(name, _)| *name);
    let mut bytes = Vec::new();
    let mut offsets = Vec::new();
    for (name, data) in &files {
        offsets.push(bytes.len() as u32);
        let mut header = [0u8; 30];
        header[..4].copy_from_slice(b"PK\x03\x04");
        put16(&mut header, 4, 20);
        put16(&mut header, 6, 0x0800);
        put16(&mut header, 8, 0);
        put16(&mut header, 12, 0x21);
        put32(&mut header, 14, crc32(data));
        put32(&mut header, 18, data.len() as u32);
        put32(&mut header, 22, data.len() as u32);
        put16(&mut header, 26, name.len() as u16);
        bytes.extend_from_slice(&header);
        bytes.extend_from_slice(name.as_bytes());
        bytes.extend_from_slice(data);
    }
    let central_start = bytes.len();
    for ((name, data), offset) in files.iter().zip(offsets) {
        let mut header = [0u8; 46];
        header[..4].copy_from_slice(b"PK\x01\x02");
        put16(&mut header, 4, 0x031e);
        put16(&mut header, 6, 20);
        put16(&mut header, 8, 0x0800);
        put16(&mut header, 10, 0);
        put16(&mut header, 14, 0x21);
        put32(&mut header, 16, crc32(data));
        put32(&mut header, 20, data.len() as u32);
        put32(&mut header, 24, data.len() as u32);
        put16(&mut header, 28, name.len() as u16);
        put32(&mut header, 38, 0x81a40000);
        put32(&mut header, 42, offset);
        bytes.extend_from_slice(&header);
        bytes.extend_from_slice(name.as_bytes());
    }
    let central_size = bytes.len() - central_start;
    let mut end = [0u8; 22];
    end[..4].copy_from_slice(b"PK\x05\x06");
    put16(&mut end, 8, files.len() as u16);
    put16(&mut end, 10, files.len() as u16);
    put32(&mut end, 12, central_size as u32);
    put32(&mut end, 16, central_start as u32);
    bytes.extend_from_slice(&end);
    bytes
}

fn signed_context(data: &std::path::Path, trusted_key: [u8; 32]) -> UpdaterContext {
    UpdaterContext {
        config: Updater {
            endpoint: "http://127.0.0.1/".into(),
            public_key: STANDARD.encode(trusted_key),
        },
        app_id: "org.example.app".into(),
        version: "1.0.0".into(),
        trusted_key,
        data: data.to_path_buf(),
    }
}
fn offered(version: &str) -> document::Document {
    document::Document {
        version: version.into(),
        notes: None,
        package: "http://127.0.0.1/package".into(),
        sha256: String::new(),
    }
}
async fn verify_bytes(
    data: &std::path::Path,
    config: &UpdaterContext,
    version: &str,
    package: &[u8],
) -> Result<(), AlefError> {
    let storage = storage::Storage::prepare(data).unwrap();
    let mut temp = storage.temp().unwrap();
    std::io::Write::write_all(temp.as_file_mut(), package).unwrap();
    tokio::time::timeout(
        WAIT,
        storage::verify_and_publish(storage, temp, config, &offered(version)),
    )
    .await
    .expect("package verification timed out")
}

#[tokio::test]
async fn signed_packages_publish_replace_older_and_preserve_noclobber_bytes() {
    let scratch = tempfile::tempdir().unwrap();
    let (older, key) = signed_package("2.0.0", "org.example.app");
    let (newer, newer_key) = signed_package("3.0.0", "org.example.app");
    assert_eq!(key, newer_key);
    let config = signed_context(scratch.path(), key);
    verify_bytes(scratch.path(), &config, "2.0.0", &older)
        .await
        .unwrap();
    let old_path = scratch.path().join("updates/2.0.0.alef");
    assert_eq!(std::fs::read(&old_path).unwrap(), older);

    verify_bytes(scratch.path(), &config, "3.0.0", &newer)
        .await
        .unwrap();
    let updates = scratch.path().join("updates");
    assert_eq!(std::fs::read(updates.join("3.0.0.alef")).unwrap(), newer);
    assert!(!old_path.exists());
    let published = std::fs::read(updates.join("3.0.0.alef")).unwrap();
    let error = verify_bytes(scratch.path(), &config, "3.0.0", &newer)
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::UpdateAlreadyDownloaded);
    assert_eq!(
        std::fs::read(updates.join("3.0.0.alef")).unwrap(),
        published
    );
}

#[tokio::test]
async fn a_signed_package_with_one_tampered_payload_byte_is_rejected_and_removed() {
    let scratch = tempfile::tempdir().unwrap();
    let (mut package, key) = signed_package("2.0.0", "org.example.app");
    let index = package
        .windows(b"Updater fixture".len())
        .position(|window| window == b"Updater fixture")
        .expect("signed package payload contains the fixture needle");
    package[index] ^= 1;
    let config = signed_context(scratch.path(), key);

    let error = verify_bytes(scratch.path(), &config, "2.0.0", &package)
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::UpdateSignatureInvalid);

    let entries: Vec<_> = std::fs::read_dir(scratch.path().join("updates"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(entries, [std::ffi::OsString::from(".alef-update.lock")]);
}

#[tokio::test]
async fn tampered_candidate_is_ignored_during_retention_and_new_verified_update_publishes() {
    let scratch = tempfile::tempdir().unwrap();
    let (candidate, key) = signed_package("2.0.0", "org.example.app");
    let (newer, _) = signed_package("3.0.0", "org.example.app");
    let config = signed_context(scratch.path(), key);
    verify_bytes(scratch.path(), &config, "2.0.0", &candidate)
        .await
        .unwrap();
    let candidate_path = scratch.path().join("updates/2.0.0.alef");
    std::fs::write(&candidate_path, b"tampered candidate").unwrap();

    verify_bytes(scratch.path(), &config, "3.0.0", &newer)
        .await
        .unwrap();

    assert_eq!(
        std::fs::read(&candidate_path).unwrap(),
        b"tampered candidate"
    );
    assert_eq!(
        std::fs::read(scratch.path().join("updates/3.0.0.alef")).unwrap(),
        newer
    );
}

#[tokio::test]
async fn signed_packages_reject_wrong_manifest_id_and_untrusted_key_without_artifacts() {
    for (manifest_id, expected) in [
        ("org.other.app", ErrorCode::UpdateIdMismatch),
        ("org.example.app", ErrorCode::UpdateKeyMismatch),
    ] {
        let scratch = tempfile::tempdir().unwrap();
        let (package, signing_key) = signed_package("2.0.0", manifest_id);
        let trusted_key = if expected == ErrorCode::UpdateKeyMismatch {
            Ed25519KeyPair::from_seed_unchecked(&[43u8; 32])
                .unwrap()
                .public_key()
                .as_ref()
                .try_into()
                .unwrap()
        } else {
            signing_key
        };
        let config = signed_context(scratch.path(), trusted_key);
        let error = verify_bytes(scratch.path(), &config, "2.0.0", &package)
            .await
            .unwrap_err();
        assert_eq!(error.code, expected);
        let entries: Vec<_> = std::fs::read_dir(scratch.path().join("updates"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        assert_eq!(entries, [std::ffi::OsString::from(".alef-update.lock")]);
    }
}
