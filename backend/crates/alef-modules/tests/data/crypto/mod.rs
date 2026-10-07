// SPDX-License-Identifier: MIT OR Apache-2.0
//! `crypto` through the registry, against the published test vectors.
use alef_core::{registry::command::Reply, AlefError, ErrorCode};
use base64::{engine::general_purpose::STANDARD, Engine};
use bytes::Bytes;
use serde_json::{json, Value};

use crate::common::Fixture;

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn unhex(text: &str) -> Vec<u8> {
    (0..text.len())
        .step_by(2)
        .map(|at| u8::from_str_radix(&text[at..at + 2], 16).unwrap())
        .collect()
}

fn b64(bytes: &[u8]) -> String {
    STANDARD.encode(bytes)
}

async fn app() -> Fixture {
    Fixture::new(None, &[]).await
}

async fn bytes_of(app: &Fixture, command: &str, args: Value, body: &[u8]) -> Vec<u8> {
    match app
        .call_reply(command, args, Some(Bytes::copy_from_slice(body)))
        .await
        .unwrap()
    {
        Reply::Bytes(bytes) => bytes.to_vec(),
        other => panic!("expected bytes, got {other:?}"),
    }
}

async fn invalid(app: &Fixture, command: &str, args: Value, body: &[u8]) {
    let error = app
        .call_reply(command, args.clone(), Some(Bytes::copy_from_slice(body)))
        .await
        .unwrap_err();
    assert_eq!(error.code, ErrorCode::InvalidArgument, "{command} {args}");
}

async fn valid(app: &Fixture, command: &str, args: Value, body: &[u8]) -> bool {
    match app
        .call_reply(command, args, Some(Bytes::copy_from_slice(body)))
        .await
        .unwrap()
    {
        Reply::Json(value) => value["valid"].as_bool().unwrap(),
        other => panic!("expected JSON, got {other:?}"),
    }
}

async fn digest(app: &Fixture, name: &str, data: &[u8]) -> String {
    hex(&bytes_of(app, "crypto.digest", json!({ "algorithm": name }), data).await)
}

async fn hmac(app: &Fixture, name: &str, key: &str, data: &[u8]) -> Vec<u8> {
    bytes_of(
        app,
        "crypto.hmac",
        json!({ "algorithm": name, "key": key }),
        data,
    )
    .await
}

async fn hmac_valid(app: &Fixture, key: &str, tag: &[u8], data: &[u8]) -> bool {
    let args = json!({ "algorithm": "sha-256", "key": key, "tag": b64(tag) });
    valid(app, "crypto.hmacVerify", args, data).await
}

async fn pbkdf2(app: &Fixture, algorithm: &str, iterations: u32, length: u32) -> String {
    let args = json!({ "algorithm": algorithm, "salt": b64(b"salt"), "iterations": iterations, "length": length });
    hex(&bytes_of(app, "crypto.pbkdf2", args, b"password").await)
}

async fn argon2id(
    app: &Fixture,
    memory: u64,
    iterations: u64,
    parallelism: u64,
    length: u64,
) -> Vec<u8> {
    let args = json!({
        "salt": b64(b"somesalt"), "memoryKiB": memory, "iterations": iterations,
        "parallelism": parallelism, "length": length,
    });
    bytes_of(app, "crypto.argon2id", args, b"password").await
}

async fn seal(app: &Fixture, cipher: &str, key: &str, aad: Option<&str>, plain: &[u8]) -> Vec<u8> {
    let mut args = json!({ "algorithm": cipher, "key": key });
    if let Some(aad) = aad {
        args["aad"] = json!(b64(aad.as_bytes()));
    }
    bytes_of(app, "crypto.seal", args, plain).await
}

async fn open(
    app: &Fixture,
    cipher: &str,
    key: &str,
    aad: Option<&str>,
    sealed: &[u8],
) -> Result<Vec<u8>, AlefError> {
    let mut args = json!({ "algorithm": cipher, "key": key });
    if let Some(aad) = aad {
        args["aad"] = json!(b64(aad.as_bytes()));
    }
    match app
        .call_reply("crypto.open", args, Some(Bytes::copy_from_slice(sealed)))
        .await?
    {
        Reply::Bytes(bytes) => Ok(bytes.to_vec()),
        other => panic!("expected bytes, got {other:?}"),
    }
}

async fn ed_sign(app: &Fixture, seed: &[u8], message: &[u8]) -> Vec<u8> {
    bytes_of(
        app,
        "crypto.ed25519Sign",
        json!({ "privateKey": b64(seed) }),
        message,
    )
    .await
}

async fn ed_valid(app: &Fixture, public: &[u8], signature: &[u8], message: &[u8]) -> bool {
    let args = json!({ "publicKey": b64(public), "signature": b64(signature) });
    valid(app, "crypto.ed25519Verify", args, message).await
}

mod ciphers;
mod passwords;

#[tokio::test]
async fn random_bytes_are_of_the_length_asked_and_never_the_same() {
    let app = app().await;
    let get = |length: u64| bytes_of(&app, "crypto.random", json!({ "length": length }), b"");
    assert!(get(0).await.is_empty());
    let (one, two) = (get(32).await, get(32).await);
    assert_eq!((one.len(), two.len()), (32, 32));
    assert_ne!(one, two);
    assert!(one.iter().any(|byte| *byte != 0));
    assert_eq!(get(1024 * 1024).await.len(), 1024 * 1024);
    invalid(
        &app,
        "crypto.random",
        json!({ "length": 1024 * 1024 + 1 }),
        b"",
    )
    .await;
    invalid(&app, "crypto.random", json!({}), b"").await;
}

#[tokio::test]
async fn digests_are_those_of_the_standards_and_a_name_is_exact() {
    let app = app().await;
    assert_eq!(
        digest(&app, "sha-1", b"abc").await,
        "a9993e364706816aba3e25717850c26c9cd0d89d"
    );
    assert_eq!(
        digest(&app, "sha-256", b"abc").await,
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
    assert_eq!(
        digest(&app, "sha-384", b"abc").await,
        "cb00753f45a35e8bb5a03d699ac65007272c32ab0eded1631a8b605a43ff5bed8086072ba1e7cc2358baeca134c825a7"
    );
    assert_eq!(
        digest(&app, "sha-512", b"abc").await,
        "ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f"
    );
    assert_eq!(
        digest(&app, "sha-256", b"").await,
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        digest(&app, "sha-256", &vec![b'a'; 1_000_000]).await,
        "cdc76e5c9914fb9281a1c7e284d73e67f1809a48a497200e046d39ccc7112cd0"
    );
    for bad in ["SHA-256", "sha256", "md5", ""] {
        invalid(&app, "crypto.digest", json!({ "algorithm": bad }), b"abc").await;
    }
    invalid(
        &app,
        "crypto.digest",
        json!({ "algorithm": "sha-256", "extra": 1 }),
        b"abc",
    )
    .await;
}

#[tokio::test]
async fn an_hmac_is_that_of_the_standards_and_is_checked_in_constant_time() {
    let app = app().await;
    let key = b64(b"Jefe");
    let data = b"what do ya want for nothing?";
    assert_eq!(
        hex(&hmac(&app, "sha-1", &key, data).await),
        "effcdf6ae5eb2fa2d27416d5f184df9c259a7c79"
    );
    let good = hmac(&app, "sha-256", &key, data).await;
    assert_eq!(
        hex(&good),
        "5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843"
    );
    assert_eq!(
        hex(&hmac(&app, "sha-384", &key, data).await),
        "af45d2e376484031617f78d2b58a6b1b9c7ef464f5a01b47e42ec3736322445e8e2240ca5e69e2c78b3239ecfab21649"
    );
    assert_eq!(
        hex(&hmac(&app, "sha-512", &key, data).await),
        "164b7a7bfcf819e2e395fbe73b56e0a387bd64222e831fd610270cd7ea2505549758bf75c05a994a6d034f65f8f0e6fdcaeab1a34d4a6b4b636e070a38bce737"
    );
    assert!(hmac_valid(&app, &key, &good, data).await);
    assert!(
        !hmac_valid(&app, &key, &good, b"what do ya want for nothing!").await,
        "another message"
    );
    let mut bent = good.clone();
    bent[0] ^= 1;
    assert!(!hmac_valid(&app, &key, &bent, data).await, "another tag");
    assert!(
        !hmac_valid(&app, &key, &good[..16], data).await,
        "a short tag is no tag"
    );
    invalid(
        &app,
        "crypto.hmac",
        json!({ "algorithm": "sha-256", "key": "***" }),
        data,
    )
    .await;
}

#[tokio::test]
async fn hkdf_is_that_of_rfc_5869_and_its_limits_are_kept() {
    let app = app().await;
    let ikm = vec![0x0b; 22];
    let first = bytes_of(
        &app,
        "crypto.hkdf",
        json!({
            "algorithm": "sha-256",
            "salt": b64(&unhex("000102030405060708090a0b0c")),
            "info": b64(&unhex("f0f1f2f3f4f5f6f7f8f9")),
            "length": 42,
        }),
        &ikm,
    )
    .await;
    assert_eq!(
        hex(&first),
        "3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf34007208d5b887185865"
    );
    let bare = bytes_of(
        &app,
        "crypto.hkdf",
        json!({ "algorithm": "sha-256", "length": 42 }),
        &ikm,
    )
    .await;
    assert_eq!(
        hex(&bare),
        "8da4e775a563c18f715f802a063c5a31b8a11f5c5ee1879ec3454e5f3c738d2d9d201395faa4b61a96c8"
    );
    for (hash, with_all, with_none) in [
        (
            "sha-1",
            "d6000ffb5b50bd3970b260017798fb9c8df9ce2e2c16b6cd709cca07dc3cf9cf26d6c6d750d0aaf5ac94",
            "0ac1af7002b3d761d1e55298da9d0506b9ae52057220a306e07b6b87e8df21d0ea00033de03984d34918",
        ),
        (
            "sha-384",
            "9b5097a86038b805309076a44b3a9f38063e25b516dcbf369f394cfab43685f748b6457763e4f0204fc5",
            "c8c96e710f89b0d7990bca68bcdec8cf854062e54c73a7abc743fade9b242daacc1cea5670415b52849c",
        ),
        (
            "sha-512",
            "832390086cda71fb47625bb5ceb168e4c8e26a1a16ed34d9fc7fe92c1481579338da362cb8d9f925d7cb",
            "f5fa02b18298a72a8c23898a8703472c6eb179dc204c03425c970e3b164bf90fff22d04836d0e2343bac",
        ),
    ] {
        let args = json!({
            "algorithm": hash,
            "salt": b64(&unhex("000102030405060708090a0b0c")),
            "info": b64(&unhex("f0f1f2f3f4f5f6f7f8f9")),
            "length": 42,
        });
        assert_eq!(
            hex(&bytes_of(&app, "crypto.hkdf", args, &ikm).await),
            with_all,
            "{hash} with a salt and an info"
        );
        let args = json!({ "algorithm": hash, "length": 42 });
        assert_eq!(
            hex(&bytes_of(&app, "crypto.hkdf", args, &ikm).await),
            with_none,
            "{hash} without them"
        );
    }
    let longest = bytes_of(
        &app,
        "crypto.hkdf",
        json!({ "algorithm": "sha-256", "length": 1024 }),
        &ikm,
    )
    .await;
    assert_eq!(longest.len(), 1024, "the longest key that is allowed");
    for length in [0, 1025] {
        invalid(
            &app,
            "crypto.hkdf",
            json!({ "algorithm": "sha-256", "length": length }),
            &ikm,
        )
        .await;
    }
    invalid(
        &app,
        "crypto.hkdf",
        json!({ "algorithm": "sha-256", "salt": "***", "length": 10 }),
        &ikm,
    )
    .await;
    invalid(
        &app,
        "crypto.hkdf",
        json!({ "algorithm": "sha-2", "length": 10 }),
        &ikm,
    )
    .await;
}

#[tokio::test]
async fn pbkdf2_is_that_of_the_published_vectors_and_its_limits_are_kept() {
    let app = app().await;
    assert_eq!(
        pbkdf2(&app, "sha-256", 1, 32).await,
        "120fb6cffcf8b32c43e7225256c4f837a86548c92ccc35480805987cb70be17b"
    );
    assert_eq!(
        pbkdf2(&app, "sha-256", 2, 32).await,
        "ae4d0c95af6b46d32d0adff928f06dd02a303f8ef3c251dfd6e2d85a95474c43"
    );
    assert_eq!(
        pbkdf2(&app, "sha-1", 1, 20).await,
        "0c60c80f961f0e71f3a9b524af6012062fe037a6"
    );
    for (hash, expected) in [
        (
            "sha-1",
            "ea6c014dc72d6f8ccd1ed92ace1d41f0d8de8957cae93136266537a8d7bf4b76",
        ),
        (
            "sha-384",
            "54f775c6d790f21930459162fc535dbf04a939185127016a04176a0730c6f1f4",
        ),
        (
            "sha-512",
            "e1d9c16aa681708a45f5c7c4e215ceb66e011a2e9f0040713f18aefdb866d53c",
        ),
    ] {
        assert_eq!(pbkdf2(&app, hash, 2, 32).await, expected, "{hash}");
    }
    assert_eq!(
        pbkdf2(&app, "sha-256", 1, 100).await.len(),
        200,
        "a key longer than a block of the hash"
    );
    assert_eq!(
        pbkdf2(&app, "sha-256", 1, 1024).await.len(),
        2048,
        "the longest key that is allowed"
    );
    for (iterations, length) in [(0, 32), (10_000_001, 32), (1, 0), (1, 1025)] {
        let args = json!({ "algorithm": "sha-256", "salt": b64(b"salt"), "iterations": iterations, "length": length });
        invalid(&app, "crypto.pbkdf2", args, b"password").await;
    }
}
