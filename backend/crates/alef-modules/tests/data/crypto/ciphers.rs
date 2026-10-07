// SPDX-License-Identifier: MIT OR Apache-2.0
//! The ciphers AES-GCM and ChaCha20-Poly1305 and the signatures of Ed25519.
use super::*;

#[tokio::test]
async fn a_sealed_message_is_the_nonce_the_text_and_a_tag_and_only_the_right_key_and_aad_open_it() {
    let app = app().await;
    let ciphers = [
        ("aes-128-gcm", 16),
        ("aes-256-gcm", 32),
        ("chacha20-poly1305", 32),
    ];
    for (cipher, key_length) in ciphers {
        let key = b64(&vec![7; key_length]);
        let other = b64(&vec![8; key_length]);
        for plain in [Vec::new(), b"hello".to_vec(), vec![0xa5; 1024 * 1024]] {
            let sealed = seal(&app, cipher, &key, None, &plain).await;
            assert_eq!(
                sealed.len(),
                12 + plain.len() + 16,
                "{cipher}: nonce, text, tag"
            );
            assert_ne!(
                sealed,
                seal(&app, cipher, &key, None, &plain).await,
                "{cipher}: a nonce of its own every time"
            );
            assert_eq!(
                open(&app, cipher, &key, None, &sealed).await.unwrap(),
                plain,
                "{cipher}"
            );
            if plain.len() > 1000 {
                continue;
            }
            let refused = |result: Result<Vec<u8>, AlefError>| {
                let error = result.unwrap_err();
                assert_eq!(error.code, ErrorCode::InvalidArgument, "{cipher}");
                assert!(
                    error.message.contains("authentication"),
                    "{}",
                    error.message
                );
            };
            refused(open(&app, cipher, &other, None, &sealed).await);
            refused(open(&app, cipher, &key, Some("another header"), &sealed).await);
            let with_aad = seal(&app, cipher, &key, Some("header"), &plain).await;
            refused(open(&app, cipher, &key, None, &with_aad).await);
            assert!(open(&app, cipher, &key, Some("header"), &with_aad)
                .await
                .is_ok());
            for at in [0, 12, sealed.len() - 1] {
                let mut bent = sealed.clone();
                bent[at] ^= 1;
                refused(open(&app, cipher, &key, None, &bent).await);
            }
        }
    }
}

#[tokio::test]
async fn a_message_of_another_maker_is_opened_and_what_is_not_a_cipher_or_key_is_told() {
    let app = app().await;
    // AES-256-GCM, key of zeros, nonce of zeros, a text of 16 zeros (test case 14 of the GCM specification).
    let mut sealed = vec![0; 12];
    sealed.extend(unhex(
        "cea7403d4d606b6e074ec5d3baf39d18d0d1c8a799996bf0265b98b5d48ab919",
    ));
    assert_eq!(
        open(&app, "aes-256-gcm", &b64(&[0; 32]), None, &sealed)
            .await
            .unwrap(),
        vec![0; 16]
    );
    // Messages that other makers sealed: AES-128-GCM (test case 2 of the GCM specification) and
    // ChaCha20-Poly1305 (RFC 8439, section 2.8.2).
    let rfc_text = "Ladies and Gentlemen of the class of '99: If I could offer you only one tip for the future, sunscreen would be it.";
    for (cipher, key, aad, message, plain) in [
        (
            "aes-128-gcm",
            vec![0; 16],
            None,
            "0000000000000000000000000388dace60b6a392f328c2b971b2fe78ab6e47d42cec13bdf53a67b21257bddf",
            vec![0; 16],
        ),
        (
            "chacha20-poly1305",
            unhex("808182838485868788898a8b8c8d8e8f909192939495969798999a9b9c9d9e9f"),
            Some(unhex("50515253c0c1c2c3c4c5c6c7")),
            "070000004041424344454647d31a8d34648e60db7b86afbc53ef7ec2a4aded51296e08fea9e2b5a736ee62d63dbea45e8ca9671282fafb69da92728b1a71de0a9e060b2905d6a5b67ecd3b3692ddbd7f2d778b8c9803aee328091b58fab324e4fad675945585808b4831d7bc3ff4def08e4b7a9de576d26586cec64b61161ae10b594f09e26a7e902ecbd0600691",
            rfc_text.as_bytes().to_vec(),
        ),
    ] {
        let mut args = json!({ "algorithm": cipher, "key": b64(&key) });
        if let Some(aad) = aad {
            args["aad"] = json!(b64(&aad));
        }
        assert_eq!(
            bytes_of(&app, "crypto.open", args, &unhex(message)).await,
            plain,
            "{cipher}"
        );
    }
    for (algorithm, key) in [
        ("aes-256-gcm", b64(&[0; 16])),
        ("aes-128-gcm", b64(&[0; 32])),
        ("chacha20-poly1305", b64(&[0; 16])),
        ("aes-256-gcm", "***".to_owned()),
        ("des", b64(&[0; 8])),
    ] {
        let args = json!({ "algorithm": algorithm, "key": key });
        invalid(&app, "crypto.seal", args.clone(), b"x").await;
        invalid(&app, "crypto.open", args, &sealed).await;
    }
    let args = json!({ "algorithm": "aes-256-gcm", "key": b64(&[0; 32]) });
    invalid(&app, "crypto.open", args.clone(), &sealed[..27]).await;
    for short in [&sealed[..0], &sealed[..5], &sealed[..11]] {
        let error = app
            .call_reply(
                "crypto.open",
                args.clone(),
                Some(Bytes::copy_from_slice(short)),
            )
            .await
            .unwrap_err();
        assert_eq!(
            error.code,
            ErrorCode::InvalidArgument,
            "{} bytes",
            short.len()
        );
        assert!(error.message.contains("nonce"), "{}", error.message);
    }
}

#[tokio::test]
async fn ed25519_signs_as_rfc_8032_does_and_a_generated_pair_works() {
    let app = app().await;
    // Tests 1 and 2 of RFC 8032, section 7.1.
    let first = ed_sign(
        &app,
        &unhex("9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60"),
        b"",
    )
    .await;
    assert_eq!(
        hex(&first),
        "e5564300c360ac729086e2cc806e828a84877f1eb8e5d974d873e065224901555fb8821590a33bacc61e39701cf9b46bd25bf5f0595bbe24655141438e7a100b"
    );
    let signature = ed_sign(
        &app,
        &unhex("4ccd089b28ff96da9db6c346ec114e0f5b8a319f35aba624da8cf6ed4fb8a6fb"),
        &[0x72],
    )
    .await;
    assert_eq!(
        hex(&signature),
        "92a009a9f0d4cab8720e820b5f642540a2b27b5416503f8fb3762223ebdb69da085ac1e43e15996e458f3613d0f11d8c387b2eaeb4302aeeb00d291612bb0c00"
    );
    let public = unhex("3d4017c3e843895a92b70aa74d1b7ebc9c982ccf2ec4968cc0cd55f12af4660c");
    assert!(ed_valid(&app, &public, &signature, &[0x72]).await);
    assert!(
        !ed_valid(&app, &public, &signature, &[0x73]).await,
        "another message"
    );
    let mut bent = signature.clone();
    bent[10] ^= 1;
    assert!(
        !ed_valid(&app, &public, &bent, &[0x72]).await,
        "another signature"
    );
    assert!(
        !ed_valid(&app, &public[..16], &signature, &[0x72]).await,
        "no public key, no valid signature"
    );

    let made = app
        .call("crypto.ed25519Generate", Value::Null)
        .await
        .unwrap();
    let seed = STANDARD
        .decode(made["privateKey"].as_str().unwrap())
        .unwrap();
    let key = STANDARD
        .decode(made["publicKey"].as_str().unwrap())
        .unwrap();
    assert_eq!((seed.len(), key.len()), (32, 32));
    let again = app
        .call("crypto.ed25519Generate", Value::Null)
        .await
        .unwrap();
    assert_ne!(made["privateKey"], again["privateKey"]);
    let proof = ed_sign(&app, &seed, b"text").await;
    assert_eq!(proof.len(), 64);
    assert!(ed_valid(&app, &key, &proof, b"text").await);
    for bad in [b64(&[0; 31]), b64(&[0; 33]), "***".to_owned()] {
        invalid(
            &app,
            "crypto.ed25519Sign",
            json!({ "privateKey": bad }),
            b"x",
        )
        .await;
    }
}
