// SPDX-License-Identifier: MIT OR Apache-2.0
use crate::{
    archive::{MAX_ENTRIES, MAX_HASHES_SIZE},
    name::{comparison_key, validate_name},
    PackageError,
};
use sha2::{Digest, Sha256};
use std::collections::{BTreeMap, BTreeSet};

pub(crate) fn digest(bytes: &[u8]) -> [u8; 32] {
    Sha256::digest(bytes).into()
}

pub(crate) fn hex(bytes: &[u8]) -> String {
    const HEX: &[u8; 16] = b"0123456789abcdef";
    bytes
        .iter()
        .flat_map(|b| {
            [
                char::from(HEX[(b >> 4) as usize]),
                char::from(HEX[(b & 15) as usize]),
            ]
        })
        .collect()
}

pub(crate) fn verify_hash(bytes: &[u8], expected: &[u8; 32]) -> Result<(), PackageError> {
    if &digest(bytes) != expected {
        return Err(PackageError::HashMismatch);
    }
    Ok(())
}

pub(crate) fn parse<'a>(
    bytes: &[u8],
    names: impl Iterator<Item = &'a str>,
) -> Result<BTreeMap<String, [u8; 32]>, PackageError> {
    let invalid = PackageError::InvalidHashList;
    if bytes.len() as u64 > MAX_HASHES_SIZE {
        return Err(invalid("size limit"));
    }
    // A CR or a BOM lands in a name or in a digit and is refused there.
    if bytes.is_empty() || !bytes.ends_with(b"\n") {
        return Err(invalid("requires nonempty LF-terminated lines"));
    }
    let mut hashes = BTreeMap::new();
    let mut seen = BTreeSet::new();
    let mut previous: Option<&str> = None;
    for (index, line) in bytes[..bytes.len() - 1].split(|b| *b == b'\n').enumerate() {
        if index >= MAX_ENTRIES {
            return Err(invalid("line count limit"));
        }
        if line.len() < 67 || &line[64..66] != b"  " {
            return Err(invalid("expected hash, two spaces and name"));
        }
        let mut hash = [0; 32];
        let nibble = |b: u8| match b {
            b'0'..=b'9' => Ok(b - b'0'),
            b'a'..=b'f' => Ok(b - b'a' + 10),
            _ => Err(invalid("hash must be 64 lowercase hex digits")),
        };
        for (out, pair) in hash.iter_mut().zip(line[..64].chunks_exact(2)) {
            *out = nibble(pair[0])? * 16 + nibble(pair[1])?;
        }
        let name = validate_name(&line[66..])?;
        if previous.is_some_and(|p| p.as_bytes() >= name.as_bytes()) {
            return Err(invalid("names must be strictly byte-sorted"));
        }
        if !seen.insert(comparison_key(name)) {
            return Err(PackageError::DuplicateName(name.to_owned()));
        }
        previous = Some(name);
        hashes.insert(name.to_owned(), hash);
    }
    let payloads: BTreeSet<&str> = names.filter(|n| !n.starts_with("META-INF/")).collect();
    for name in hashes.keys() {
        if !payloads.contains(name.as_str()) {
            return Err(PackageError::ExtraHash(name.clone()));
        }
    }
    for name in payloads {
        if !hashes.contains_key(name) {
            return Err(PackageError::MissingHash(name.to_owned()));
        }
    }
    Ok(hashes)
}
