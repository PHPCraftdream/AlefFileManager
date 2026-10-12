// SPDX-License-Identifier: MIT OR Apache-2.0
use super::{list, signature, Signer};
use crate::PackageError;
use base64::{engine::general_purpose::STANDARD, Engine};
use ed25519_dalek::{Signer as _, SigningKey};

fn record(key: &[u8], sig: &[u8]) -> Vec<u8> {
    format!(
        "alef-package 1\nkey {}\nsig {}\n",
        STANDARD.encode(key),
        STANDARD.encode(sig)
    )
    .into_bytes()
}
fn signed(hashes: &[u8]) -> (Vec<u8>, [u8; 32]) {
    // RFC 8032 test-vector seed, deterministic and public.
    let seed = unhex("9d61b19deffd5a60ba844af492ec2cc44449c5697b326919703bac031cae7f60");
    let key = SigningKey::from_bytes(&seed.try_into().unwrap());
    let message = [signature::DOMAIN, hashes].concat();
    (
        record(
            key.verifying_key().as_bytes(),
            &key.sign(&message).to_bytes(),
        ),
        key.verifying_key().to_bytes(),
    )
}
fn unhex(s: &str) -> Vec<u8> {
    s.as_bytes()
        .chunks_exact(2)
        .map(|p| u8::from_str_radix(std::str::from_utf8(p).unwrap(), 16).unwrap())
        .collect()
}
fn line(name: &str) -> String {
    format!("{}  {name}\n", "0".repeat(64))
}

#[test]
fn sha256_matches_known_answers() {
    assert_eq!(
        list::hex(&list::digest(b"")),
        "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855"
    );
    assert_eq!(
        list::hex(&list::digest(b"abc")),
        "ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad"
    );
}
#[test]
fn signed_identity_has_the_known_key_fingerprint() {
    let (bytes, key) = signed(b"exact list\n");
    assert_eq!(
        list::hex(&key),
        "d75a980182b10ab7d54bfed3c964073a0ee172f3daa62325af021a68f707511a"
    );
    assert_eq!(
        signature::verify(&bytes, b"exact list\n").unwrap(),
        Signer::Signed {
            key,
            fingerprint: "21fe31dfa154a261626bf854046fd2271b7bed4b6abe45aa58877ef47f9721b9".into()
        }
    );
}
#[test]
fn hash_lists_require_exact_coverage_and_byte_order() {
    let text = line("Z") + &line("alef.ktav") + &line("z");
    assert!(matches!(
        list::parse(text.as_bytes(), ["Z", "alef.ktav", "z"].into_iter()),
        Err(PackageError::DuplicateName(_))
    ));
    let text = line("Z") + &line("alef.ktav");
    assert!(list::parse(
        text.as_bytes(),
        ["alef.ktav", "Z", "META-INF/hashes"].into_iter()
    )
    .is_ok());
    assert!(matches!(
        list::parse(
            line("alef.ktav").as_bytes(),
            ["alef.ktav", "asset"].into_iter()
        ),
        Err(PackageError::MissingHash(_))
    ));
    assert!(matches!(
        list::parse(text.as_bytes(), ["alef.ktav"].into_iter()),
        Err(PackageError::ExtraHash(_))
    ));
    assert!(list::parse(
        (line("META-INF/hashes") + &line("alef.ktav")).as_bytes(),
        ["alef.ktav", "META-INF/hashes"].into_iter()
    )
    .is_err());
}
#[test]
fn malformed_hash_lists_are_rejected() {
    let good = line("alef.ktav");
    let cases = vec![
        String::new(),
        good.trim_end().into(),
        format!("\u{feff}{good}"),
        good.replace('\n', "\r\n"),
        format!("{good}\n"),
        good.replace("  ", " "),
        good.replace("  ", "   "),
        good.replacen('0', "A", 1),
        good.replacen('0', "g", 1),
        line("../bad"),
        line("a b"),
        line("CON"),
        line("é"),
        line("Alef.ktav"),
        format!("{good}{good}"),
        line("b") + &good,
    ];
    for text in cases {
        assert!(
            list::parse(text.as_bytes(), ["alef.ktav"].into_iter()).is_err(),
            "{text:?}"
        );
    }
    assert!(list::parse(&[0xff, b'\n'], ["alef.ktav"].into_iter()).is_err());
}
#[test]
fn hash_lists_exceeding_the_line_limit_are_rejected() {
    let text: String = (0..10_001).map(|i| line(&format!("x{i:05}"))).collect();
    assert!(matches!(
        list::parse(text.as_bytes(), std::iter::empty()),
        Err(PackageError::InvalidHashList("line count limit"))
    ));
}
#[test]
fn malformed_signature_structure_and_base64_are_rejected() {
    let (good, _) = signed(b"list\n");
    let good = String::from_utf8(good).unwrap();
    for text in [
        good.trim_end().to_owned(),
        format!("{good}\n"),
        good.replace('\n', "\r\n"),
        format!("\u{feff}{good}"),
        good.replace("package 1", "package 2"),
        good.replace("key ", "key  "),
        good.replace("sig ", "signature "),
        good.replace('=', ""),
        good.replace("key ", "key !"),
    ] {
        assert!(
            signature::verify(text.as_bytes(), b"list\n").is_err(),
            "{text:?}"
        );
    }
    // Key is 32 bytes, so its last base64 sextet has two unused bits.
    let mut noncanonical = good.into_bytes();
    let last = 15 + 4 + 42;
    let alphabet = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let i = alphabet
        .iter()
        .position(|b| *b == noncanonical[last])
        .unwrap();
    noncanonical[last] = alphabet[i + 1];
    assert!(signature::verify(&noncanonical, b"list\n").is_err());
    for (key, sig) in [
        (vec![0; 31], vec![0; 64]),
        (vec![0; 32], vec![0; 63]),
        (vec![0; 33], vec![0; 65]),
    ] {
        assert!(signature::verify(&record(&key, &sig), b"list\n").is_err());
    }
}
#[test]
fn signatures_with_wrong_messages_keys_or_bytes_are_rejected() {
    let (bytes, key) = signed(b"list\n");
    assert!(matches!(
        signature::verify(&bytes, b"list"),
        Err(PackageError::SignatureMismatch)
    ));
    let text = String::from_utf8(bytes).unwrap();
    let raw = STANDARD
        .decode(text.lines().nth(2).unwrap().strip_prefix("sig ").unwrap())
        .unwrap();
    let other = SigningKey::from_bytes(&[7; 32]).verifying_key().to_bytes();
    assert!(matches!(
        signature::verify(&record(&other, &raw), b"list\n"),
        Err(PackageError::SignatureMismatch)
    ));
    let mut bad = raw;
    bad[40] ^= 1;
    assert!(matches!(
        signature::verify(&record(&key, &bad), b"list\n"),
        Err(PackageError::SignatureMismatch)
    ));
}
#[test]
fn small_order_keys_and_r_and_noncanonical_s_are_rejected() {
    let (bytes, key) = signed(b"list\n");
    let text = String::from_utf8(bytes).unwrap();
    let raw = STANDARD
        .decode(text.lines().nth(2).unwrap().strip_prefix("sig ").unwrap())
        .unwrap();
    for weak in [[0; 32], {
        let mut k = [0; 32];
        k[0] = 1;
        k
    }] {
        assert!(matches!(
            signature::verify(&record(&weak, &raw), b"list\n"),
            Err(PackageError::InvalidSigningKey)
        ));
    }
    for r in [[0; 32], {
        let mut r = [0; 32];
        r[0] = 1;
        r
    }] {
        let mut sig = raw.clone();
        sig[..32].copy_from_slice(&r);
        assert!(matches!(
            signature::verify(&record(&key, &sig), b"list\n"),
            Err(PackageError::SignatureMismatch)
        ));
    }
    let mut sig = raw;
    sig[32..].copy_from_slice(&unhex(
        "edd3f55c1a631258d69cf7a2def9de1400000000000000000000000000000010",
    ));
    assert!(matches!(
        signature::verify(&record(&key, &sig), b"list\n"),
        Err(PackageError::SignatureMismatch)
    ));
    sig[32..].fill(255);
    assert!(signature::verify(&record(&key, &sig), b"list\n").is_err());
}
#[test]
fn malformed_compressed_key_has_distinct_error() {
    // y=2 is not a decompressible Edwards point.
    let mut key = [0; 32];
    key[0] = 2;
    assert!(matches!(
        signature::verify(&record(&key, &[0; 64]), b"list\n"),
        Err(PackageError::InvalidSigningKey)
    ));
}

#[test]
fn strict_verification_rejects_a_forgery_that_ordinary_verification_accepts() {
    use ed25519_dalek::{Signature, Verifier, VerifyingKey};
    // Derived from dalek 2.2.0 tests/validation_criteria.rs: ordinary verification
    // permits LowOrderR, strict does not. A=B+(0,-1) has order 2L, not small order;
    // R=identity and S=SHA512(R||A||DOMAIN||hashes) mod L. For this even S,
    // [S]B-[S]A=identity, so ordinary verification accepts the forged signature.
    let key: [u8; 32] = unhex("9599999999999999999999999999999999999999999999999999999999999999")
        .try_into()
        .unwrap();
    let raw: [u8; 64] = unhex(
        "0100000000000000000000000000000000000000000000000000000000000000483bdd5d3912eeb3cab40c95315c84a6d5dd53e2108999c1962cfc4032e5a00f",
    ).try_into().unwrap();
    let hashes = b"forged list 2
";
    let message = [signature::DOMAIN, hashes].concat();
    let key = VerifyingKey::from_bytes(&key).unwrap();
    let sig = Signature::from_bytes(&raw);
    assert!(!key.is_weak());
    assert!(key.verify(&message, &sig).is_ok());
    assert!(key.verify_strict(&message, &sig).is_err());
    assert!(matches!(
        signature::verify(&record(key.as_bytes(), &raw), hashes),
        Err(PackageError::SignatureMismatch)
    ));
}
