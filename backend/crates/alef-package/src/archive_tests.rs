// SPDX-License-Identifier: MIT OR Apache-2.0
use super::*;
use std::io::{Cursor, Write};

#[derive(Clone)]
struct Record {
    name: Vec<u8>,
    payload: Vec<u8>,
    size: u32,
    method: u16,
    flags: u16,
    crc: u32,
}
impl Record {
    fn new(name: &str, payload: &[u8]) -> Self {
        Self {
            name: name.as_bytes().to_vec(),
            payload: payload.to_vec(),
            size: payload.len() as u32,
            method: 0,
            flags: 0x0800,
            crc: crc32fast::hash(payload),
        }
    }
    fn deflated(name: &str, payload: &[u8]) -> Self {
        let mut record = Self::new(name, payload);
        let mut encoder =
            flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
        encoder.write_all(payload).expect("fixture compression");
        record.payload = encoder.finish().expect("fixture finish");
        record.method = 8;
        record
    }
}
fn put16(bytes: &mut [u8], p: usize, value: u16) {
    bytes[p..p + 2].copy_from_slice(&value.to_le_bytes());
}
fn put32(bytes: &mut [u8], p: usize, value: u32) {
    bytes[p..p + 4].copy_from_slice(&value.to_le_bytes());
}

// No ZIP writer: every local/CD field and arbitrary byte range remains independently mutable.
struct Fixture {
    bytes: Vec<u8>,
    locals: Vec<usize>,
    centrals: Vec<usize>,
    eocd: usize,
}
fn handmade(records: &[Record]) -> Fixture {
    let mut bytes = Vec::new();
    let mut locals = Vec::new();
    let mut centrals = Vec::new();
    for r in records {
        locals.push(bytes.len());
        let mut h = [0; 30];
        h[..4].copy_from_slice(b"PK\x03\x04");
        put16(&mut h, 4, 20);
        put16(&mut h, 6, r.flags);
        put16(&mut h, 8, r.method);
        put32(&mut h, 14, r.crc);
        put32(&mut h, 18, r.payload.len() as u32);
        put32(&mut h, 22, r.size);
        put16(&mut h, 26, r.name.len() as u16);
        bytes.extend_from_slice(&h);
        bytes.extend_from_slice(&r.name);
        bytes.extend_from_slice(&r.payload);
    }
    let start = bytes.len();
    for (r, offset) in records.iter().zip(&locals) {
        centrals.push(bytes.len());
        let mut h = [0; 46];
        h[..4].copy_from_slice(b"PK\x01\x02");
        put16(&mut h, 4, 0x031e);
        put16(&mut h, 6, 20);
        put16(&mut h, 8, r.flags);
        put16(&mut h, 10, r.method);
        put32(&mut h, 16, r.crc);
        put32(&mut h, 20, r.payload.len() as u32);
        put32(&mut h, 24, r.size);
        put16(&mut h, 28, r.name.len() as u16);
        put32(&mut h, 38, 0x81a40000);
        put32(&mut h, 42, *offset as u32);
        bytes.extend_from_slice(&h);
        bytes.extend_from_slice(&r.name);
    }
    let eocd = bytes.len();
    let mut h = [0; 22];
    h[..4].copy_from_slice(b"PK\x05\x06");
    put16(&mut h, 8, records.len() as u16);
    put16(&mut h, 10, records.len() as u16);
    put32(&mut h, 12, (eocd - start) as u32);
    put32(&mut h, 16, start as u32);
    bytes.extend_from_slice(&h);
    Fixture {
        bytes,
        locals,
        centrals,
        eocd,
    }
}
fn base() -> Fixture {
    handmade(&[
        Record::new("alef.ktav", b"manifest"),
        Record::new("assets/a.js", b"data"),
    ])
}
fn rejects(f: Fixture) -> PackageError {
    match inspect_archive(Cursor::new(f.bytes)) {
        Err(e) => e,
        Ok(_) => panic!("expected rejection"),
    }
}
fn read_error(record: Record) -> PackageError {
    let f = handmade(&[Record::new("alef.ktav", b""), record]);
    let mut archive = inspect_archive(Cursor::new(f.bytes)).expect("container valid");
    archive.read_entry(1).expect_err("payload invalid")
}

#[test]
fn stored_deflate_and_empty_payloads_round_trip() {
    for payload in [b"".as_slice(), b"hello hello hello", &[0; 128 * 1024]] {
        let records = [
            Record::new("alef.ktav", b"manifest"),
            Record::new("stored", payload),
            Record::deflated("deflated", payload),
        ];
        let f = handmade(&records);
        let mut archive = inspect_archive(Cursor::new(f.bytes)).expect("strict archive");
        assert_eq!(archive.entries().len(), 3);
        assert_eq!(archive.read_entry(0).expect("CRC"), b"manifest");
        assert_eq!(archive.read_entry(1).expect("stored CRC"), payload);
        assert_eq!(archive.read_entry(2).expect("deflate CRC"), payload);
        assert!(archive.read_entry(3).is_err());
    }
}

#[test]
fn ascii_case_insensitive_duplicate_names_are_rejected() {
    for names in [["a", "a"], ["a", "A"]] {
        let f = handmade(&[
            Record::new("alef.ktav", b""),
            Record::new(names[0], b""),
            Record::new(names[1], b""),
        ]);
        assert!(matches!(rejects(f), PackageError::DuplicateName(_)));
    }
}

#[test]
fn file_and_path_prefix_conflicts_are_rejected() {
    for names in [["a", "A/b"], ["a/b", "A"], ["a/b/c", "A/B"]] {
        let f = handmade(&[
            Record::new("alef.ktav", b""),
            Record::new(names[0], b""),
            Record::new(names[1], b""),
        ]);
        assert!(matches!(rejects(f), PackageError::PathConflict(_)));
    }
}

#[test]
fn forbidden_and_misspelled_reserved_names_are_rejected() {
    for name in [
        "dir/",
        "שלום",
        "Ä.js",
        "a b",
        "Alef.ktav",
        "META-INF/HASHES",
        "meta-inf/signature",
        "mEtA-InF/foreign",
    ] {
        assert!(matches!(
            rejects(handmade(&[Record::new(name, b"")])),
            PackageError::InvalidName(_, _)
        ));
    }
}

#[test]
fn archives_without_a_manifest_are_rejected() {
    assert!(matches!(
        rejects(handmade(&[Record::new("other", b"")])),
        PackageError::MissingManifest
    ));
}

#[test]
fn exactly_spelled_metadata_names_are_accepted() {
    assert!(inspect_archive(Cursor::new(
        handmade(&[
            Record::new("alef.ktav", b""),
            Record::new("META-INF/hashes", b""),
            Record::new("META-INF/signature", b"")
        ])
        .bytes
    ))
    .is_ok());
}

#[test]
fn local_and_central_fields_must_match() {
    for field in [4, 6, 8, 10, 12, 14, 18, 22, 26, 28] {
        let mut f = base();
        f.bytes[f.locals[1] + field] ^= 1;
        rejects(f);
    }
    let mut f = base();
    f.bytes[f.locals[1] + 30] ^= 1;
    rejects(f);
    for field in [6, 8, 10, 12, 14, 16, 20, 24, 28, 30, 32, 34, 42] {
        let mut f = base();
        f.bytes[f.centrals[1] + field] ^= 1;
        rejects(f);
    }
}

#[test]
fn a_local_version_other_than_the_central_one_is_rejected() {
    let mut f = base();
    put16(&mut f.bytes, f.locals[1] + 4, 10);
    rejects(f);
}

#[test]
fn flags_other_than_exact_utf8_are_rejected() {
    for flags in [0, 1, 8, 0x0801, 0x0808, 0x0840, 0x2800, 0x1800, 0xffff] {
        let mut r = Record::new("alef.ktav", b"");
        r.flags = flags;
        rejects(handmade(&[r]));
    }
}

#[test]
fn unsupported_compression_methods_are_rejected() {
    for method in [1, 9, 12, 93, 99, u16::MAX] {
        let mut r = Record::new("alef.ktav", b"");
        r.method = method;
        rejects(handmade(&[r]));
    }
}

#[test]
fn multidisk_and_inconsistent_eocd_fields_are_rejected() {
    for field in [4, 6, 8, 10, 20] {
        let mut f = base();
        put16(&mut f.bytes, f.eocd + field, 1);
        rejects(f);
    }
}

#[test]
fn zip64_eocd_size_and_offset_sentinels_are_rejected() {
    for field in [12, 16] {
        let mut f = base();
        put32(&mut f.bytes, f.eocd + field, u32::MAX);
        assert!(matches!(rejects(f), PackageError::Zip64));
    }
}

#[test]
fn zip64_central_field_sentinels_are_rejected() {
    for field in [20, 24, 42] {
        let mut f = base();
        put32(&mut f.bytes, f.centrals[0] + field, u32::MAX);
        rejects(f);
    }
}

#[test]
fn central_extras_comments_and_disk_numbers_are_rejected() {
    for field in [30, 32, 34] {
        let mut f = base();
        put16(&mut f.bytes, f.centrals[0] + field, 1);
        rejects(f);
    }
}

#[test]
fn local_extra_fields_are_rejected() {
    let mut f = base();
    put16(&mut f.bytes, f.locals[0] + 28, 1);
    rejects(f);
}

#[test]
fn eocd_comments_are_rejected() {
    let mut f = base();
    put16(&mut f.bytes, f.eocd + 20, 4);
    f.bytes.extend_from_slice(b"note");
    rejects(f);
}

#[test]
fn archives_with_prefixes_are_rejected() {
    let mut f = base();
    f.bytes.insert(0, 0);
    rejects(f);
}

#[test]
fn archives_with_suffixes_are_rejected() {
    let mut f = base();
    f.bytes.push(0);
    rejects(f);
}

#[test]
fn archives_with_adjusted_prefixes_are_rejected() {
    let mut f = base();
    // Fully adjust all offsets for a prefix: contiguity still rejects it.
    f.bytes.insert(0, 0);
    for (i, c) in f.centrals.iter().enumerate() {
        put32(&mut f.bytes, c + 1 + 42, (f.locals[i] + 1) as u32);
    }
    put32(&mut f.bytes, f.eocd + 1 + 16, (f.centrals[0] + 1) as u32);
    rejects(f);
}

#[test]
fn overlapping_local_offsets_are_rejected() {
    let mut f = base();
    put32(&mut f.bytes, f.centrals[1] + 42, 0);
    rejects(f);
}

#[test]
fn misaligned_and_out_of_bounds_local_offsets_are_rejected() {
    for offset in [1, u32::MAX - 1] {
        let mut f = base();
        put32(&mut f.bytes, f.centrals[1] + 42, offset);
        rejects(f);
    }
}

#[test]
fn reordered_central_records_are_rejected() {
    let mut f = base();
    let first = f.bytes[f.centrals[0]..f.centrals[1]].to_vec();
    let second = f.bytes[f.centrals[1]..f.eocd].to_vec();
    f.bytes
        .splice(f.centrals[0]..f.eocd, second.into_iter().chain(first));
    rejects(f);
}

#[test]
fn concatenated_archives_are_rejected() {
    let mut f = base();
    let other = base();
    f.bytes.extend(other.bytes);
    rejects(f);
    // Gap before central, with a consistent EOCD location.
}

#[test]
fn gaps_before_the_central_directory_are_rejected() {
    let mut f = base();
    f.bytes.insert(f.centrals[0], 0);
    put32(&mut f.bytes, f.eocd + 1 + 16, (f.centrals[0] + 1) as u32);
    rejects(f);
}

#[test]
fn external_attributes_are_ignored_not_used_as_file_type() {
    for attrs in [0, 0x10, 0xa1ff0000, 0x41ed0010, u32::MAX] {
        let mut f = base();
        put32(&mut f.bytes, f.centrals[0] + 38, attrs);
        assert!(inspect_archive(Cursor::new(f.bytes)).is_ok());
    }
}

#[test]
fn all_truncations_and_unbounded_name_lengths_reject_without_panics() {
    let original = base();
    for n in 0..original.bytes.len() {
        assert!(inspect_archive(Cursor::new(&original.bytes[..n])).is_err());
    }
    for field in [28, 30, 32] {
        let mut f = base();
        put16(&mut f.bytes, f.centrals[0] + field, u16::MAX);
        rejects(f);
    }
    for position in [0, original.centrals[0], original.eocd] {
        let mut f = base();
        f.bytes[position] = 0;
        rejects(f);
    }
    assert!(matches!(
        add(u64::MAX, 1),
        Err(PackageError::ArithmeticOverflow)
    ));
    assert!(u32_at(&[], usize::MAX).is_err());
}

#[test]
fn declared_limits_are_checked_before_payload_allocation() {
    assert!(check_size("file", MAX_ENTRY_SIZE).is_ok());
    assert!(check_size("file", MAX_ENTRY_SIZE + 1).is_err());
    for (name, limit) in [
        ("META-INF/hashes", MAX_HASHES_SIZE),
        ("META-INF/signature", MAX_SIGNATURE_SIZE),
    ] {
        assert!(check_size(name, limit).is_ok());
        assert!(matches!(
            check_size(name, limit + 1),
            Err(PackageError::MetadataSizeLimit(_, _))
        ));
    }
    for (name, size) in [
        ("alef.ktav", MAX_ENTRY_SIZE + 1),
        ("META-INF/hashes", MAX_HASHES_SIZE + 1),
        ("META-INF/signature", MAX_SIGNATURE_SIZE + 1),
    ] {
        let mut r = Record::deflated(name, b"");
        r.size = size as u32;
        rejects(handmade(&[r]));
    }
    let mut f = base();
    put32(&mut f.bytes, f.eocd + 12, (MAX_CENTRAL_SIZE + 1) as u32);
    assert!(matches!(rejects(f), PackageError::CentralSizeLimit));
    let mut records = vec![Record::new("alef.ktav", b"")];
    records.extend((1..MAX_ENTRIES).map(|i| Record::new(&format!("file{i}"), b"")));
    assert!(inspect_archive(Cursor::new(handmade(&records).bytes)).is_ok());
    records.push(Record::new("one-more", b""));
    assert!(matches!(
        rejects(handmade(&records)),
        PackageError::EntryCountLimit
    ));
    let mut records: Vec<_> = ["alef.ktav", "b", "c", "d", "e"]
        .map(|name| Record::deflated(name, b""))
        .into();
    for r in &mut records {
        r.size = MAX_ENTRY_SIZE as u32;
    }
    assert!(
        matches!(rejects(handmade(&records)), PackageError::Entry(_, e) if matches!(*e, PackageError::TotalSizeLimit))
    );
}

#[test]
fn archive_length_limit_is_checked_before_reads_or_allocations() {
    struct Huge;
    impl Read for Huge {
        fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
            panic!("must not read");
        }
    }
    impl Seek for Huge {
        fn seek(&mut self, _: SeekFrom) -> std::io::Result<u64> {
            Ok(MAX_ARCHIVE_SIZE + 1)
        }
    }
    assert!(matches!(
        inspect_archive(Huge),
        Err(PackageError::ArchiveSizeLimit)
    ));
}

#[test]
fn payload_reads_require_valid_crc_and_exact_deflate_sizes() {
    let mut r = Record::new("file", b"hello");
    r.crc ^= 1;
    assert!(
        matches!(read_error(r), PackageError::Entry(_, e) if matches!(*e, PackageError::CrcMismatch))
    );
    let mut r = Record::deflated("file", b"hello");
    r.crc ^= 1;
    read_error(r);
    for size in [0, 4, 6] {
        let mut r = Record::deflated("file", b"hello");
        r.size = size;
        read_error(r);
    }
    let mut r = Record::deflated("file", b"hello");
    r.payload.push(0);
    read_error(r);
    let mut r = Record::deflated("file", b"hello");
    r.payload.extend(Record::deflated("x", b"world").payload);
    read_error(r);
    let original = Record::deflated("file", b"hello");
    for n in 0..original.payload.len() {
        let mut r = original.clone();
        r.payload.truncate(n);
        read_error(r);
    }
    let mut r = Record::deflated("file", b"hello");
    r.payload = vec![0xff; 10];
    read_error(r);
    let mut r = Record::new("alef.ktav", b"hello");
    r.size += 1;
    rejects(handmade(&[r]));
}

#[test]
fn reordered_local_records_are_rejected() {
    let mut f = base();
    let first = f.bytes[..f.locals[1]].to_vec();
    let second = f.bytes[f.locals[1]..f.centrals[0]].to_vec();
    let second_len = second.len();
    f.bytes
        .splice(..f.centrals[0], second.into_iter().chain(first));
    put32(&mut f.bytes, f.centrals[0] + 42, second_len as u32);
    put32(&mut f.bytes, f.centrals[1] + 42, 0);
    rejects(f);
}

#[test]
fn unindexed_local_records_are_rejected() {
    let mut f = base();
    let other = handmade(&[Record::new("hidden", b"hidden")]);
    let local = &other.bytes[..other.centrals[0]];
    f.bytes
        .splice(f.centrals[0]..f.centrals[0], local.iter().copied());
    put32(
        &mut f.bytes,
        f.eocd + local.len() + 16,
        (f.centrals[0] + local.len()) as u32,
    );
    rejects(f);
}

#[test]
fn local_payloads_overlapping_the_central_directory_are_rejected() {
    let mut f = base();
    for at in [
        f.locals[1] + 18,
        f.locals[1] + 22,
        f.centrals[1] + 20,
        f.centrals[1] + 24,
    ] {
        put32(&mut f.bytes, at, 100);
    }
    rejects(f);
}

#[test]
fn data_descriptors_are_rejected() {
    let mut f = base();
    let descriptor = b"PK\x07\x08\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00\x00";
    f.bytes
        .splice(f.centrals[0]..f.centrals[0], descriptor.iter().copied());
    put32(
        &mut f.bytes,
        f.eocd + descriptor.len() + 16,
        (f.centrals[0] + descriptor.len()) as u32,
    );
    rejects(f);
    let mut r = Record::new("alef.ktav", b"");
    r.flags |= 8;
    rejects(handmade(&[r]));
}

#[test]
fn eocd_comments_containing_eocd_magic_are_rejected() {
    let mut f = base();
    let fake = f.bytes[f.eocd..].to_vec();
    put16(&mut f.bytes, f.eocd + 20, fake.len() as u16);
    f.bytes.extend(fake);
    rejects(f);
}

#[test]
fn zip64_eocd_records_and_locators_are_rejected() {
    let mut f = base();
    let mut zip64 = vec![0; 76];
    zip64[..4].copy_from_slice(b"PK\x06\x06");
    zip64[56..60].copy_from_slice(b"PK\x06\x07");
    f.bytes.splice(f.eocd..f.eocd, zip64);
    rejects(f);
}

#[test]
fn integer_field_reads_require_complete_in_bounds_fields() {
    assert_eq!(u16_at(&[1, 2], 0).unwrap(), 513);
    assert_eq!(u32_at(&[1, 2, 3, 4], 0).unwrap(), 0x04030201);
    for at in [1, 2, usize::MAX] {
        assert!(u16_at(&[1, 2], at).is_err());
        assert!(u32_at(&[1, 2, 3, 4], at).is_err());
    }
}

#[test]
fn total_declared_size_accepts_the_limit_and_rejects_one_more_byte() {
    let mut records: Vec<_> = ["alef.ktav", "b", "c", "d"]
        .map(|n| Record::deflated(n, b""))
        .into();
    for r in &mut records {
        r.size = MAX_ENTRY_SIZE as u32;
    }
    assert!(inspect_archive(Cursor::new(handmade(&records).bytes)).is_ok());
    records.push(Record::deflated("e", b"x"));
    assert!(
        matches!(rejects(handmade(&records)), PackageError::Entry(_, e) if matches!(*e, PackageError::TotalSizeLimit))
    );
}
