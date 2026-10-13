// SPDX-License-Identifier: MIT OR Apache-2.0
mod support;
use alef_package::{Package, PackageError, Signer};
use std::{
    cell::RefCell,
    io::{self, Cursor, Read, Seek, SeekFrom},
    rc::Rc,
};

#[test]
fn entry_sizes_are_exact_and_limited_reads_respect_boundaries_for_stored_and_deflated_entries() {
    let files: &[(&str, &[u8])] = &[
        ("alef.ktav", b"manifest"),
        ("asset", b"plumless"),
        ("empty", b""),
    ];
    let hashes = support::hashes(files);
    let signature = support::signature(&hashes);
    let mut records = files.to_vec();
    records.push(("META-INF/hashes", &hashes));
    records.push(("META-INF/signature", &signature));
    for bytes in [support::zip(&records), support::deflated_zip(&records)] {
        let mut package = Package::from_bytes(bytes).unwrap();
        let identity = package.signer().clone();
        for &(name, body) in &records {
            let size = body.len() as u64;
            assert_eq!(package.entry_size(name), Some(size));
            if size != 0 {
                for maximum in [0, size - 1] {
                    let error = package.read_limited(name, maximum).unwrap_err();
                    assert!(error.to_string().contains(name));
                    assert!(error.to_string().contains(&maximum.to_string()));
                    match error {
                        PackageError::Entry(entry, inner) => {
                            assert_eq!(entry, name);
                            assert!(
                                matches!(*inner, PackageError::ReadSizeLimit(n) if n == maximum)
                            );
                        }
                        other => panic!("wrong limit error: {other:?}"),
                    }
                }
            }
            for maximum in [size, size + 1, u64::MAX] {
                assert_eq!(package.read_limited(name, maximum).unwrap(), body);
            }
            assert_eq!(package.read(name).unwrap(), body);
        }
        for name in ["ASSET", "META-INF/HASHES", "missing", "", "asset/"] {
            assert_eq!(package.entry_size(name), None);
            assert!(matches!(
                package.read_limited(name, 0),
                Err(PackageError::EntryNotFound(entry)) if entry == name
            ));
        }
        assert_eq!(package.signer(), &identity);
    }
}

#[test]
fn entry_sizes_and_limited_reads_use_fixed_table_after_header_mutation() {
    let (mut package, bytes) = shared(true, true);
    let sizes: Vec<_> = package
        .names()
        .map(|name| (name.to_owned(), package.entry_size(name).unwrap()))
        .collect();
    let mut changed = bytes.borrow_mut();
    // Destroy every local and central declared-size field, but leave payloads.
    let locals: Vec<_> = changed
        .windows(4)
        .enumerate()
        .filter_map(|(at, w)| (w == b"PK\x03\x04").then_some(at))
        .collect();
    let centrals: Vec<_> = changed
        .windows(4)
        .enumerate()
        .filter_map(|(at, w)| (w == b"PK\x01\x02").then_some(at))
        .collect();
    assert_eq!(locals.len(), sizes.len());
    assert_eq!(centrals.len(), sizes.len());
    for at in locals {
        changed[at + 22..at + 26].fill(0xff);
    }
    for at in centrals {
        changed[at + 24..at + 28].fill(0xff);
    }
    drop(changed);
    for (name, size) in sizes {
        assert_eq!(package.entry_size(&name), Some(size));
        assert!(matches!(
            root(&package.read_limited(&name, size - 1).unwrap_err()),
            PackageError::ReadSizeLimit(n) if *n == size - 1
        ));
        assert_eq!(
            package.read_limited(&name, size).unwrap().len() as u64,
            size
        );
    }
}

#[test]
fn limited_reads_still_verify_full_crc_sha_and_metadata_snapshot() {
    for (hashes, signed) in [(false, false), (true, false), (true, true)] {
        let (mut package, bytes) = shared(hashes, signed);
        let mut changed = bytes.borrow_mut();
        let at = changed.windows(8).position(|w| w == b"plumless").unwrap();
        // Last byte: a limited read must not return a verified-looking prefix.
        changed[at + 7] ^= 1;
        drop(changed);
        assert!(matches!(
            root(&package.read_limited("asset", 8).unwrap_err()),
            PackageError::CrcMismatch
        ));
        assert!(matches!(
            root(&package.read_limited("asset", 7).unwrap_err()),
            PackageError::ReadSizeLimit(7)
        ));
    }
    for signed in [false, true] {
        let (mut package, bytes) = shared(true, signed);
        let mut changed = bytes.borrow_mut();
        let at = changed.windows(8).position(|w| w == b"plumless").unwrap();
        changed[at..at + 8].copy_from_slice(b"buckeroo");
        drop(changed);
        assert!(matches!(
            root(&package.read_limited("asset", 8).unwrap_err()),
            PackageError::HashMismatch
        ));
    }
    let (mut package, bytes) = shared(true, true);
    let signature = support::signature(&support::hashes(FILES));
    let altered = collide(&signature, 20);
    let mut changed = bytes.borrow_mut();
    let at = changed
        .windows(signature.len())
        .position(|w| w == signature)
        .unwrap();
    changed[at..at + signature.len()].copy_from_slice(&altered);
    drop(changed);
    assert!(matches!(
        root(
            &package
                .read_limited("META-INF/signature", signature.len() as u64)
                .unwrap_err()
        ),
        PackageError::MetadataChanged
    ));
}

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
        // One counter per process: a name is never reused while another test deletes its directory,
        // which Windows answers with "Access is denied" instead of "already exists".
        static NEXT: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        loop {
            let sequence = NEXT.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let path = root.join(format!("{}-{sequence}", std::process::id()));
            match std::fs::create_dir(&path) {
                Ok(()) => return Self(path),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => panic!("scratch directory: {e}"),
            }
        }
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
fn owned_snapshot_packages_can_be_sent_between_threads() {
    fn assert_send<T: Send>() {}
    assert_send::<Package<Cursor<Vec<u8>>>>();
}

#[test]
fn snapshot_opening_accepts_ceiling_minus_one_and_exactly_the_ceiling() {
    let scratch = Scratch::new();
    let bytes = support::package(FILES, true, true);
    let length = bytes.len() as u64;
    let path = scratch.write_new("bounded.zip", &bytes);
    for limit in [length + 1, length] {
        let mut package = Package::open_snapshot(&path, limit).unwrap();
        assert_eq!(package.read("asset").unwrap(), b"plumless");
        assert!(matches!(package.signer(), Signer::Signed { .. }));
    }
    assert!(matches!(
        Package::open_snapshot(&path, length - 1),
        Err(PackageError::SnapshotSizeLimit(n)) if n == length - 1
    ));
    assert!(matches!(
        Package::open_snapshot(&path, 0),
        Err(PackageError::SnapshotSizeLimit(0))
    ));
}

#[test]
fn snapshot_opening_rejects_directory_handles_as_nonregular_files() {
    let scratch = Scratch::new();
    assert!(matches!(
        Package::open_snapshot(&scratch.0, 0),
        Err(PackageError::NotRegularFile)
    ));
}

#[cfg(any(windows, unix))]
#[test]
fn snapshot_opening_rejects_device_handles_as_nonregular_files() {
    #[cfg(windows)]
    let device = "NUL";
    #[cfg(unix)]
    let device = "/dev/null";
    let error = match Package::open_snapshot(device, 0) {
        Err(error) => error,
        Ok(_) => panic!("device accepted as a package"),
    };
    assert!(matches!(error, PackageError::NotRegularFile), "{error:?}");
}

#[test]
fn snapshot_opening_preserves_missing_path_io_errors() {
    let scratch = Scratch::new();
    assert!(matches!(
        Package::open_snapshot(scratch.0.join("missing.zip"), 1024),
        Err(PackageError::Io(e)) if e.kind() == io::ErrorKind::NotFound
    ));
}

fn assert_original_snapshot(package: &mut Package<Cursor<Vec<u8>>>, signer: &Signer) {
    for _ in 0..2 {
        assert_eq!(package.read("alef.ktav").unwrap(), b"manifest");
        assert_eq!(package.read("asset").unwrap(), b"plumless");
        assert_eq!(package.signer(), signer);
    }
}

#[test]
fn snapshot_reads_are_independent_of_original_path_replacement() {
    let scratch = Scratch::new();
    let bytes = support::package(FILES, true, true);
    let path = scratch.write_new("original.zip", &bytes);
    let mut package = Package::open_snapshot(&path, bytes.len() as u64).unwrap();
    let signer = package.signer().clone();
    let moved = scratch.0.join("moved.zip");
    std::fs::rename(&path, &moved).unwrap();
    scratch.write_new(
        "original.zip",
        &support::package(&[("alef.ktav", b"replacement")], false, false),
    );
    // Mutate the original inode too: retaining its live handle is not a snapshot.
    std::fs::OpenOptions::new()
        .write(true)
        .open(&moved)
        .unwrap()
        .set_len(0)
        .unwrap();
    assert_original_snapshot(&mut package, &signer);
    let mut next = Package::open_snapshot(&path, 1024).unwrap();
    assert_eq!(next.read("alef.ktav").unwrap(), b"replacement");
    assert_eq!(next.signer(), &Signer::Unsigned);
}

#[test]
fn snapshot_reads_are_independent_of_original_file_truncation() {
    let scratch = Scratch::new();
    let bytes = support::package(FILES, true, true);
    let path = scratch.write_new("truncated.zip", &bytes);
    let mut package = Package::open_snapshot(&path, bytes.len() as u64).unwrap();
    let signer = package.signer().clone();
    std::fs::OpenOptions::new()
        .write(true)
        .open(&path)
        .unwrap()
        .set_len(0)
        .unwrap();
    assert_eq!(std::fs::metadata(&path).unwrap().len(), 0);
    assert_original_snapshot(&mut package, &signer);
    assert!(matches!(
        Package::open_snapshot(&path, 1024),
        Err(PackageError::InvalidArchive(_))
    ));
}

#[test]
fn snapshot_reads_are_independent_of_same_size_original_file_overwrites() {
    use std::io::Write;
    let scratch = Scratch::new();
    let bytes = support::package(FILES, true, true);
    let replacement = support::package(
        &[("alef.ktav", b"changed!"), ("asset", b"buckeroo")],
        true,
        true,
    );
    assert_eq!(bytes.len(), replacement.len());
    let path = scratch.write_new("overwritten.zip", &bytes);
    let mut package = Package::open_snapshot(&path, bytes.len() as u64).unwrap();
    let signer = package.signer().clone();
    let mut file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
    file.write_all(&replacement).unwrap();
    drop(file);
    assert_eq!(std::fs::metadata(&path).unwrap().len(), bytes.len() as u64);
    assert_original_snapshot(&mut package, &signer);
    let mut next = Package::open_snapshot(&path, replacement.len() as u64).unwrap();
    assert_eq!(next.read("alef.ktav").unwrap(), b"changed!");
    assert_eq!(next.read("asset").unwrap(), b"buckeroo");
}

#[test]
fn snapshot_opening_keeps_the_full_payload_verification_pass() {
    let scratch = Scratch::new();
    let mut bytes = support::package(FILES, false, false);
    let at = bytes.windows(8).position(|w| w == b"plumless").unwrap();
    bytes[at] ^= 1;
    let path = scratch.write_new("bad-payload.zip", &bytes);
    match Package::open_snapshot(&path, bytes.len() as u64) {
        Err(e) => assert!(matches!(root(&e), PackageError::CrcMismatch)),
        Ok(_) => panic!("snapshot opening skipped a payload CRC check"),
    }
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
