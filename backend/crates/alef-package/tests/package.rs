// SPDX-License-Identifier: MIT OR Apache-2.0
mod support;
use alef_package::{Package, PackageError, Signer};
use std::{
    cell::RefCell,
    io::{self, Cursor, Read, Seek, SeekFrom},
    rc::Rc,
};

const FILES: &[(&str, &[u8])] = &[("alef.ktav", b"manifest"), ("asset", b"plumless")];
fn root(error: &PackageError) -> &PackageError {
    match error {
        PackageError::Entry(_, inner) => root(inner),
        _ => error,
    }
}
fn error(bytes: Vec<u8>) -> PackageError {
    match Package::from_bytes(bytes) {
        Err(e) => e,
        Ok(_) => panic!("invalid package accepted"),
    }
}
#[test]
fn signed_and_unsigned_packages_expose_verified_owned_payloads() {
    for (hashes, signed) in [(false, false), (true, false), (true, true)] {
        let bytes = support::package(FILES, hashes, signed);
        assert_eq!(bytes, support::package(FILES, hashes, signed));
        let mut package = Package::from_bytes(bytes.as_slice()).unwrap();
        assert_eq!(matches!(package.signer(), Signer::Signed { .. }), signed);
        let signer = package.signer().clone();
        let names: Vec<_> = package.names().collect();
        assert!(names.contains(&"alef.ktav"));
        assert!(names.contains(&"asset"));
        assert_eq!(names.len(), 2 + usize::from(hashes) + usize::from(signed));
        for (name, data) in FILES {
            assert_eq!(package.read(name).unwrap(), *data);
        }
        assert_eq!(package.signer(), &signer);
        assert!(matches!(
            package.read("missing"),
            Err(PackageError::EntryNotFound(_))
        ));
        assert!(package.read("ASSET").is_err());
        if hashes {
            assert_eq!(
                package.read("META-INF/hashes").unwrap(),
                support::hashes(FILES)
            );
        }
    }
}
#[test]
fn signatures_without_hashes_are_rejected() {
    assert!(matches!(
        error(support::package(FILES, false, true)),
        PackageError::MissingHashes
    ));
}
#[test]
fn opening_checks_every_payload_crc_even_without_hashes() {
    for (hashes, signed) in [(false, false), (true, false), (true, true)] {
        let mut bytes = support::package(FILES, hashes, signed);
        let at = bytes.windows(8).position(|w| w == b"plumless").unwrap();
        bytes[at] ^= 1;
        assert!(matches!(root(&error(bytes)), PackageError::CrcMismatch));
    }
}
#[test]
fn opening_checks_every_payload_sha_even_with_valid_crc() {
    assert_eq!(crc32fast::hash(b"plumless"), crc32fast::hash(b"buckeroo"));
    for signed in [false, true] {
        let mut bytes = support::package(FILES, true, signed);
        assert!(Package::from_bytes(bytes.as_slice()).is_ok());
        let at = bytes.windows(8).position(|w| w == b"plumless").unwrap();
        bytes[at..at + 8].copy_from_slice(b"buckeroo");
        assert!(matches!(root(&error(bytes)), PackageError::HashMismatch));
    }
}
#[test]
fn public_api_rejects_extra_missing_and_malformed_hashes() {
    for list in [
        support::hashes(&FILES[..1]),
        support::hashes(&[
            ("alef.ktav", b"manifest"),
            ("asset", b"plumless"),
            ("extra", b""),
        ]),
        b"bad\n".to_vec(),
    ] {
        let sig = support::signature(&list);
        let bytes = support::zip(&[
            ("alef.ktav", b"manifest"),
            ("asset", b"plumless"),
            ("META-INF/hashes", &list),
            ("META-INF/signature", &sig),
        ]);
        assert!(matches!(
            root(&error(bytes)),
            PackageError::MissingHash(_)
                | PackageError::ExtraHash(_)
                | PackageError::InvalidHashList(_)
        ));
    }
}
#[test]
fn public_api_signature_covers_exact_list_bytes() {
    let list = support::hashes(FILES);
    let sig = support::signature(&list[..list.len() - 1]);
    assert!(matches!(
        error(support::zip(&[
            ("alef.ktav", b"manifest"),
            ("asset", b"plumless"),
            ("META-INF/hashes", &list),
            ("META-INF/signature", &sig)
        ])),
        PackageError::SignatureMismatch
    ));
}
struct SharedReader {
    bytes: Rc<RefCell<Vec<u8>>>,
    position: u64,
}
impl Read for SharedReader {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let bytes = self.bytes.borrow();
        let mut cursor = Cursor::new(bytes.as_slice());
        cursor.set_position(self.position);
        let n = cursor.read(out)?;
        self.position = cursor.position();
        Ok(n)
    }
}
impl Seek for SharedReader {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let bytes = self.bytes.borrow();
        let mut cursor = Cursor::new(bytes.as_slice());
        cursor.set_position(self.position);
        self.position = cursor.seek(from)?;
        Ok(self.position)
    }
}
fn shared(hashes: bool, signed: bool) -> (Package<SharedReader>, Rc<RefCell<Vec<u8>>>) {
    let bytes = Rc::new(RefCell::new(support::package(FILES, hashes, signed)));
    let reader = SharedReader {
        bytes: Rc::clone(&bytes),
        position: 0,
    };
    (Package::from_reader(reader).unwrap(), bytes)
}
#[test]
fn reads_recheck_sha_from_fixed_hash_map_and_keep_signer() {
    for signed in [false, true] {
        let (mut package, bytes) = shared(true, signed);
        let identity = package.signer().clone();
        let mut changed = bytes.borrow_mut();
        let at = changed.windows(8).position(|w| w == b"plumless").unwrap();
        changed[at..at + 8].copy_from_slice(b"buckeroo");
        drop(changed);
        assert!(matches!(
            root(&package.read("asset").unwrap_err()),
            PackageError::HashMismatch
        ));
        assert_eq!(package.signer(), &identity);
        assert_eq!(package.read("alef.ktav").unwrap(), b"manifest");
    }
}
#[test]
fn unsigned_packages_without_hashes_have_a_crc_only_contract() {
    let (mut package, bytes) = shared(false, false);
    let mut changed = bytes.borrow_mut();
    let at = changed.windows(8).position(|w| w == b"plumless").unwrap();
    changed[at..at + 8].copy_from_slice(b"buckeroo");
    drop(changed);
    assert_eq!(package.read("asset").unwrap(), b"buckeroo");
    assert_eq!(package.signer(), &Signer::Unsigned);
}
#[test]
fn reads_recheck_crc_and_do_not_reparse_table_or_metadata() {
    let (mut package, bytes) = shared(true, true);
    let names: Vec<_> = package.names().map(str::to_owned).collect();
    let identity = package.signer().clone();
    let mut changed = bytes.borrow_mut();
    let at = changed.windows(8).position(|w| w == b"plumless").unwrap();
    changed[at] ^= 1;
    let sig = support::signature(&support::hashes(FILES));
    let at = changed.windows(sig.len()).position(|w| w == sig).unwrap();
    changed[at] ^= 1;
    // Corrupt EOCD too: reads must never inspect a new archive table.
    let end = changed.len();
    changed[end - 22] ^= 1;
    drop(changed);
    assert!(matches!(
        root(&package.read("asset").unwrap_err()),
        PackageError::CrcMismatch
    ));
    assert!(matches!(
        root(&package.read("META-INF/signature").unwrap_err()),
        PackageError::CrcMismatch
    ));
    assert_eq!(package.names().collect::<Vec<_>>(), names);
    assert_eq!(package.signer(), &identity);
    assert_eq!(package.read("alef.ktav").unwrap(), b"manifest");
}
// The last 4 bytes that give the data the CRC-32 target (the CRC is a bijection of them).
fn forged_tail(prefix: &[u8], target: u32) -> [u8; 4] {
    let mut table = [0u32; 256];
    for (i, slot) in table.iter_mut().enumerate() {
        let mut c = i as u32;
        for _ in 0..8 {
            c = if c & 1 != 0 {
                0xEDB8_8320 ^ (c >> 1)
            } else {
                c >> 1
            };
        }
        *slot = c;
    }
    let mut by_top = [0u8; 256];
    for (i, entry) in table.iter().enumerate() {
        by_top[(entry >> 24) as usize] = i as u8;
    }
    let mut rest = !target;
    let i4 = by_top[(rest >> 24) as usize];
    rest ^= table[i4 as usize];
    let i3 = by_top[((rest << 8) >> 24) as usize];
    rest ^= table[i3 as usize] >> 8;
    let i2 = by_top[((rest << 16) >> 24) as usize];
    rest ^= table[i2 as usize] >> 16;
    let i1 = by_top[((rest << 24) >> 24) as usize];
    let mut register = !crc32fast::hash(prefix);
    let mut tail = [0u8; 4];
    for (slot, index) in tail.iter_mut().zip([i1, i2, i3, i4]) {
        *slot = index ^ (register as u8);
        register = (register >> 8) ^ table[index as usize];
    }
    tail
}
// Changes the byte at `at` of `content` and keeps its CRC-32 by rewriting the last 4 bytes.
fn collide(content: &[u8], at: usize) -> Vec<u8> {
    let mut changed = content.to_vec();
    changed[at] ^= 1;
    let end = changed.len() - 4;
    let tail = forged_tail(&changed[..end], crc32fast::hash(content));
    changed[end..].copy_from_slice(&tail);
    assert_eq!(crc32fast::hash(&changed), crc32fast::hash(content));
    assert_ne!(changed, content);
    changed
}
#[test]
fn metadata_changed_after_the_open_with_the_same_crc_is_not_returned() {
    let (mut package, bytes) = shared(true, true);
    let signature = support::signature(&support::hashes(FILES));
    let altered = collide(&signature, 20);
    let mut archive = bytes.borrow_mut();
    let at = archive
        .windows(signature.len())
        .position(|w| w == signature)
        .unwrap();
    archive[at..at + signature.len()].copy_from_slice(&altered);
    drop(archive);
    assert!(matches!(
        root(&package.read("META-INF/signature").unwrap_err()),
        PackageError::MetadataChanged
    ));
    assert_eq!(package.read("alef.ktav").unwrap(), b"manifest");
}
#[test]
fn a_payload_changed_after_the_open_with_the_same_crc_fails_its_hash() {
    let (mut package, bytes) = shared(true, true);
    let altered = collide(b"manifest", 1);
    let mut archive = bytes.borrow_mut();
    let at = archive.windows(8).position(|w| w == b"manifest").unwrap();
    archive[at..at + 8].copy_from_slice(&altered);
    drop(archive);
    assert!(matches!(
        root(&package.read("alef.ktav").unwrap_err()),
        PackageError::HashMismatch
    ));
}
#[test]
fn read_returns_owned_snapshot_not_a_live_mapping() {
    let (mut package, bytes) = shared(true, true);
    let snapshot = package.read("asset").unwrap();
    bytes.borrow_mut().fill(0);
    assert_eq!(snapshot, b"plumless");
    assert!(package.read("asset").is_err());
}
struct Scratch(std::path::PathBuf);
impl Scratch {
    fn new() -> Self {
        let root = std::path::Path::new(env!("CARGO_TARGET_TMPDIR")).join("alef-package");
        std::fs::create_dir_all(&root).unwrap();
        for sequence in 0.. {
            let path = root.join(format!("{}-{sequence}", std::process::id()));
            match std::fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => panic!("scratch directory: {e}"),
            }
        }
        unreachable!()
    }
    fn write_new(&self, name: &str, bytes: &[u8]) -> std::path::PathBuf {
        use std::io::Write;
        let path = self.0.join(name);
        let mut file = std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&path)
            .unwrap();
        file.write_all(bytes).unwrap();
        path
    }
}
impl Drop for Scratch {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.0);
    }
}

#[test]
fn scratch_files_are_cleaned_up_during_unwinding() {
    let mut path = None;
    assert!(std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let scratch = Scratch::new();
        scratch.write_new("unwind.zip", b"fixture");
        path = Some(scratch.0.clone());
        panic!("exercise cleanup");
    }))
    .is_err());
    assert!(!path.unwrap().exists());
}

#[test]
fn file_specialization_retains_one_file_handle() {
    let scratch = Scratch::new();
    let path = scratch.write_new("original.zip", &support::package(FILES, true, true));
    let mut package = Package::open(&path).unwrap();
    let moved = scratch.0.join("moved.zip");
    std::fs::rename(&path, &moved).unwrap();
    scratch.write_new(
        "original.zip",
        &support::package(&[("alef.ktav", b"replacement")], false, false),
    );
    assert_eq!(package.read("alef.ktav").unwrap(), b"manifest");
    assert!(matches!(package.signer(), Signer::Signed { .. }));
}

#[test]
fn reads_reject_changed_deflate_streams_that_expand_beyond_the_fixed_declared_cap() {
    use std::io::Write;
    let original: Vec<u8> = (0..128).collect();
    let bytes = Rc::new(RefCell::new(support::deflated_zip(&[
        ("alef.ktav", b"manifest"),
        ("asset", &original),
    ])));
    let mut package = Package::from_reader(SharedReader {
        bytes: Rc::clone(&bytes),
        position: 0,
    })
    .unwrap();
    let names: Vec<_> = package.names().map(str::to_owned).collect();
    let mut encoder =
        flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
    encoder.write_all(&vec![0; 64 * 1024]).unwrap();
    let replacement = encoder.finish().unwrap();
    let mut changed = bytes.borrow_mut();
    let central = changed.windows(4).position(|w| w == b"PK\x01\x02").unwrap();
    let first_size = u32::from_le_bytes(changed[18..22].try_into().unwrap()) as usize;
    let local = 30 + "alef.ktav".len() + first_size;
    let offset = local + 30 + "asset".len();
    let cap = u32::from_le_bytes(changed[local + 18..local + 22].try_into().unwrap()) as usize;
    assert!(replacement.len() <= cap);
    changed[offset..offset + replacement.len()].copy_from_slice(&replacement);
    // Alter both size declarations too; reads must use the opening table's cap.
    for at in [local + 22, central + 46 + "alef.ktav".len() + 24] {
        changed[at..at + 4].copy_from_slice(&(64 * 1024_u32).to_le_bytes());
    }
    drop(changed);
    assert!(matches!(
        root(&package.read("asset").unwrap_err()),
        PackageError::PayloadSizeMismatch
    ));
    assert_eq!(package.names().collect::<Vec<_>>(), names);
    assert_eq!(package.signer(), &Signer::Unsigned);
    assert_eq!(package.read("alef.ktav").unwrap(), b"manifest");
}
