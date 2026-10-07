// SPDX-License-Identifier: MIT OR Apache-2.0
//! The password hashes Argon2id and scrypt against the published vectors, and their limits.
use super::*;

#[tokio::test]
async fn argon2id_hashes_as_the_reference_does_and_asks_no_more_than_it_may() {
    let app = app().await;
    // `argon2 -id -t 2 -m 16 -p 1 -l 32` of the reference program, for "password" and "somesalt".
    assert_eq!(
        hex(&argon2id(&app, 65536, 2, 1, 32).await),
        "09316115d5cf24ed5a15a31a3ba326e5cf32edc24702987c02b6566f61913cf7"
    );
    let small = argon2id(&app, 64, 3, 2, 24).await;
    assert_eq!(small.len(), 24);
    assert_eq!(
        small,
        argon2id(&app, 64, 3, 2, 24).await,
        "the same arguments, the same hash"
    );
    assert_ne!(
        small,
        argon2id(&app, 64, 3, 1, 24).await,
        "the lanes are in the hash"
    );
    assert_ne!(
        small,
        argon2id(&app, 64, 4, 2, 24).await,
        "the passes are in the hash"
    );
    assert_ne!(
        small,
        argon2id(&app, 128, 3, 2, 24).await,
        "the memory is in the hash"
    );
    let by_default = bytes_of(
        &app,
        "crypto.argon2id",
        json!({ "salt": b64(b"somesalt") }),
        b"password",
    )
    .await;
    assert_eq!(by_default.len(), 32);

    for bad in [
        json!({ "salt": b64(b"short") }),
        json!({ "memoryKiB": 1024 * 1024 + 1 }),
        json!({ "memoryKiB": 4 }),
        json!({ "iterations": 0 }),
        json!({ "iterations": 101 }),
        json!({ "parallelism": 0 }),
        json!({ "parallelism": 17 }),
        json!({ "length": 3 }),
        json!({ "length": 1025 }),
    ] {
        let mut args = json!({ "salt": b64(b"somesalt"), "memoryKiB": 64 });
        args.as_object_mut()
            .unwrap()
            .extend(bad.as_object().unwrap().clone());
        invalid(&app, "crypto.argon2id", args, b"password").await;
    }
    let error = app
        .call_reply(
            "crypto.argon2id",
            json!({ "salt": b64(b"7 bytes"), "memoryKiB": 64 }),
            Some(Bytes::from_static(b"password")),
        )
        .await
        .unwrap_err();
    assert!(error.message.contains("at least 8"), "{}", error.message);
}

#[tokio::test]
async fn scrypt_hashes_as_rfc_7914_does_and_asks_no_more_than_it_may() {
    let app = app().await;
    let first = bytes_of(
        &app,
        "crypto.scrypt",
        json!({ "salt": "", "logN": 4, "r": 1, "p": 1, "length": 64 }),
        b"",
    )
    .await;
    assert_eq!(
        hex(&first),
        "77d6576238657b203b19ca42c18a0497f16b4844e3074ae8dfdffa3fede21442fcd0069ded0948f8326a753a0fc81f17e8d3e0fb2e0d3628cf35e20c38d18906"
    );
    let second = bytes_of(
        &app,
        "crypto.scrypt",
        json!({ "salt": b64(b"NaCl"), "logN": 10, "r": 8, "p": 16, "length": 64 }),
        b"password",
    )
    .await;
    assert_eq!(
        hex(&second),
        "fdbabe1c9d3472007856e7190d01e9fe7c6ad7cbc8237830e77376634b3731622eaf30d92e22a3886ff109279d9830dac727afb94a83ee6d8360cbdfa2cc0640"
    );
    // Any length from 1 to 1024 bytes: and a shorter key is the start of a longer one of the same arguments.
    for length in [1_u64, 9, 10, 64, 65, 100, 1024] {
        let args = json!({ "salt": "", "logN": 4, "r": 1, "p": 1, "length": length });
        let key = bytes_of(&app, "crypto.scrypt", args, b"").await;
        assert_eq!(key.len() as u64, length);
        let shared = key.len().min(first.len());
        assert_eq!(
            key[..shared],
            first[..shared],
            "{length}: the start of the key of 64 bytes"
        );
    }
    let by_default = bytes_of(
        &app,
        "crypto.scrypt",
        json!({ "salt": b64(b"somesalt"), "logN": 4 }),
        b"password",
    )
    .await;
    assert_eq!(by_default.len(), 32);
    for bad in [
        json!({ "logN": 0 }),
        json!({ "logN": 25 }),
        json!({ "logN": 24, "r": 64 }),
        json!({ "r": 0 }),
        json!({ "r": 65 }),
        json!({ "p": 0 }),
        json!({ "p": 17 }),
        json!({ "length": 0 }),
        json!({ "length": 1025 }),
    ] {
        let mut args = json!({ "salt": b64(b"somesalt"), "logN": 4 });
        args.as_object_mut()
            .unwrap()
            .extend(bad.as_object().unwrap().clone());
        invalid(&app, "crypto.scrypt", args, b"password").await;
    }
}

#[tokio::test]
async fn several_password_hashes_asked_at_once_all_come_in_their_turn() {
    let app = app().await;
    let ask = |salt: &'static str| {
        let args = json!({ "salt": b64(salt.as_bytes()), "memoryKiB": 4096, "iterations": 2 });
        let app = &app;
        async move { bytes_of(app, "crypto.argon2id", args, b"password").await }
    };
    let (a, b, c, d, again) = tokio::join!(
        ask("salt-one-"),
        ask("salt-two-"),
        ask("salt-three"),
        ask("salt-four"),
        ask("salt-one-")
    );
    assert_eq!(a, again);
    assert!([&a, &b, &c, &d].iter().all(|hash| hash.len() == 32));
    assert_ne!(a, b);
}
