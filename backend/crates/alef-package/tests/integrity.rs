// SPDX-License-Identifier: MIT OR Apache-2.0
mod support;
use alef_package::{Package, PackageError, Signer};
use base64::{engine::general_purpose::STANDARD, Engine};
use std::{fs, io, path::PathBuf};

const FILES: &[(&str, &[u8])] = &[("alef.ktav", b"manifest"), ("asset", b"plumless")];

struct Scratch(PathBuf);
impl Scratch {
    fn new() -> Self {
        let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join("alef-package-integrity");
        fs::create_dir_all(&root).unwrap();
        // One counter per process: a name is never reused while another test deletes its directory,
        // which Windows answers with "Access is denied" instead of "already exists".
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        loop {
            let sequence = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let path = root.join(format!("{}-{sequence}", std::process::id()));
            match fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => panic!("scratch directory: {e}"),
            }
        }
    }
    fn write(&self, bytes: &[u8]) -> PathBuf {
        let path = self.0.join("fixture.zip");
        fs::write(&path, bytes).unwrap();
        path
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn u32_at(bytes: &[u8], at: usize) -> usize {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap()) as usize
}

// Replace a stored payload and recompute BOTH CRC fields, without touching metadata.
fn replace_with_recomputed_crc(bytes: &mut [u8], name: &str, replacement: &[u8]) {
    let end = bytes.len() - 22;
    let mut central = u32_at(bytes, end + 16);
    while central < end {
        assert_eq!(&bytes[central..central + 4], b"PK\x01\x02");
        let length =
            u16::from_le_bytes(bytes[central + 28..central + 30].try_into().unwrap()) as usize;
        if &bytes[central + 46..central + 46 + length] == name.as_bytes() {
            let local = u32_at(bytes, central + 42);
            assert_eq!(&bytes[local..local + 4], b"PK\x03\x04");
            assert_eq!(&bytes[local + 8..local + 10], &[0, 0]);
            assert_eq!(u32_at(bytes, local + 18), replacement.len());
            assert_eq!(u32_at(bytes, local + 22), replacement.len());
            let payload = local + 30 + length;
            let old_crc = crc32fast::hash(&bytes[payload..payload + replacement.len()]);
            let new_crc = crc32fast::hash(replacement);
            assert_ne!(old_crc, new_crc, "fixture must not be a CRC collision");
            bytes[payload..payload + replacement.len()].copy_from_slice(replacement);
            for at in [local + 14, central + 16] {
                bytes[at..at + 4].copy_from_slice(&new_crc.to_le_bytes());
                assert_eq!(u32_at(bytes, at), new_crc as usize);
            }
            return;
        }
        central += 46 + length;
    }
    panic!("fixture entry missing: {name}");
}

fn root(error: &PackageError) -> &PackageError {
    match error {
        PackageError::Entry(_, inner) => root(inner),
        _ => error,
    }
}

fn assert_both_openers_reject(bytes: &[u8], expected: fn(&PackageError) -> bool) {
    let error = match Package::from_bytes(bytes) {
        Err(error) => error,
        Ok(_) => panic!("from_bytes accepted invalid integrity"),
    };
    assert!(expected(root(&error)), "{error:?}");
    let scratch = Scratch::new();
    let path = scratch.write(bytes);
    let error = match Package::open_snapshot(path, bytes.len() as u64) {
        Err(error) => error,
        Ok(_) => panic!("open_snapshot accepted invalid integrity"),
    };
    assert!(expected(root(&error)), "{error:?}");
}

#[test]
fn both_openers_reject_a_signed_payload_mutation_with_recomputed_crc_and_original_metadata() {
    let mut bytes = support::package(FILES, true, true);
    let mut original = Package::from_bytes(bytes.as_slice()).unwrap();
    let hashes = original.read("META-INF/hashes").unwrap();
    let signature = original.read("META-INF/signature").unwrap();
    drop(original);
    assert!(Package::from_bytes(support::deflated_zip(&[
        ("alef.ktav", b"manifest"),
        ("asset", b"plumless"),
        ("META-INF/hashes", &hashes),
        ("META-INF/signature", &signature),
    ]))
    .is_ok());
    replace_with_recomputed_crc(&mut bytes, "asset", b"changed!");
    assert!(bytes.windows(hashes.len()).any(|window| window == hashes));
    assert!(bytes
        .windows(signature.len())
        .any(|window| window == signature));
    assert_both_openers_reject(&bytes, |error| matches!(error, PackageError::HashMismatch));
}

#[test]
fn both_openers_reject_a_signature_bit_flip_with_canonical_base64_and_recomputed_crc() {
    let hashes = support::hashes(FILES);
    let signature = support::signature(&hashes);
    let text = std::str::from_utf8(&signature).unwrap();
    let mut lines = text.lines();
    let header = lines.next().unwrap();
    let key = lines.next().unwrap();
    let encoded = lines.next().unwrap().strip_prefix("sig ").unwrap();
    let mut decoded = STANDARD.decode(encoded).unwrap();
    assert_eq!(decoded.len(), 64);
    decoded[0] ^= 1;
    let encoded = STANDARD.encode(&decoded);
    assert_eq!(STANDARD.encode(STANDARD.decode(&encoded).unwrap()), encoded);
    let altered = format!("{header}\n{key}\nsig {encoded}\n");
    let mut bytes = support::package(FILES, true, true);
    assert!(Package::from_bytes(bytes.as_slice()).is_ok());
    // All payloads and their hashes remain valid; only the signature bytes change.
    replace_with_recomputed_crc(&mut bytes, "META-INF/signature", altered.as_bytes());
    assert!(bytes.windows(hashes.len()).any(|window| window == hashes));
    assert_both_openers_reject(&bytes, |error| {
        matches!(error, PackageError::SignatureMismatch)
    });
}

fn assert_unsigned_snapshot(with_hashes: bool) {
    let scratch = Scratch::new();
    let bytes = support::package(FILES, with_hashes, false);
    let path = scratch.write(&bytes);
    let mut package = Package::open_snapshot(&path, bytes.len() as u64).unwrap();
    assert_eq!(package.signer(), &Signer::Unsigned);
    assert_eq!(package.entry_size("META-INF/signature"), None);
    if with_hashes {
        assert_eq!(
            package.read("META-INF/hashes").unwrap(),
            support::hashes(FILES)
        );
    } else {
        assert_eq!(package.entry_size("META-INF/hashes"), None);
    }
    fs::write(&path, b"later disk mutation").unwrap();
    for &(name, payload) in FILES {
        assert_eq!(package.entry_size(name), Some(payload.len() as u64));
        assert_eq!(
            package.read_limited(name, payload.len() as u64).unwrap(),
            payload
        );
        assert_eq!(package.read(name).unwrap(), payload);
    }
    assert_eq!(package.signer(), &Signer::Unsigned);
    let mut changed = bytes;
    replace_with_recomputed_crc(&mut changed, "asset", b"changed!");
    if with_hashes {
        assert_both_openers_reject(&changed, |error| {
            matches!(error, PackageError::HashMismatch)
        });
    } else {
        let path = scratch.write(&changed);
        let mut next = Package::open_snapshot(path, changed.len() as u64).unwrap();
        assert_eq!(next.signer(), &Signer::Unsigned);
        assert_eq!(next.read("asset").unwrap(), b"changed!");
    }
}

#[test]
fn unsigned_snapshots_with_hashes_verify_sha_and_keep_owned_payloads() {
    assert_unsigned_snapshot(true);
}

#[test]
fn unsigned_snapshots_without_hashes_verify_only_crc_and_keep_owned_payloads() {
    assert_unsigned_snapshot(false);
}
