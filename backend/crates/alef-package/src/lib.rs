// SPDX-License-Identifier: MIT OR Apache-2.0
//! Strict format-1 packages: fixed archive table, CRC/SHA verification and Ed25519 signatures.
#![forbid(unsafe_code)]

mod archive;
mod error;
mod name;
mod snapshot;
mod verification;

pub use error::PackageError;
pub use verification::Signer;

use archive::{inspect_archive, Archive};
use std::{
    collections::BTreeMap,
    fs::File,
    io::{Cursor, Read, Seek},
    path::Path,
};
use verification::{list, signature};

/// Owns one reader and an immutable verified table, hash list and signer.
/// All reads return a complete CRC-verified snapshot; when hashes exist, SHA-256
/// is also checked. Unsigned packages without hashes have CRC integrity only.
pub struct Package<R: Read + Seek> {
    archive: Archive<R>,
    hashes: Option<BTreeMap<String, [u8; 32]>>,
    metadata: BTreeMap<String, Vec<u8>>,
    signer: Signer,
}

impl<R: Read + Seek> Package<R> {
    pub fn from_reader(reader: R) -> Result<Self, PackageError> {
        let mut archive = inspect_archive(reader)?;
        let mut metadata = BTreeMap::new();
        for name in ["META-INF/hashes", "META-INF/signature"] {
            if let Some(index) = archive.entries().iter().position(|e| e.name == name) {
                metadata.insert(name.to_owned(), archive.read_entry(index)?);
            }
        }
        let hashes = metadata
            .get("META-INF/hashes")
            .map(|bytes| list::parse(bytes, archive.entries().iter().map(|e| e.name.as_str())))
            .transpose()?;
        let signer = match metadata.get("META-INF/signature") {
            Some(bytes) => signature::verify(
                bytes,
                metadata
                    .get("META-INF/hashes")
                    .ok_or(PackageError::MissingHashes)?,
            )?,
            None => Signer::Unsigned,
        };
        let mut package = Self {
            archive,
            hashes,
            metadata,
            signer,
        };
        // Full payload pass, even for unsigned packages without hashes.
        for index in 0..package.archive.entries().len() {
            if !package
                .metadata
                .contains_key(&package.archive.entries()[index].name)
            {
                package.read_index(index)?;
            }
        }
        Ok(package)
    }

    pub fn signer(&self) -> &Signer {
        &self.signer
    }

    /// All archive names, including metadata, in the fixed central-directory order.
    pub fn names(&self) -> impl ExactSizeIterator<Item = &str> {
        self.archive.entries().iter().map(|e| e.name.as_str())
    }

    pub fn read(&mut self, name: &str) -> Result<Vec<u8>, PackageError> {
        let index = self
            .archive
            .entries()
            .iter()
            .position(|e| e.name == name)
            .ok_or_else(|| PackageError::EntryNotFound(name.to_owned()))?;
        self.read_index(index)
    }

    /// Returns the fixed table's declared uncompressed size for an exact entry name.
    pub fn entry_size(&self, name: &str) -> Option<u64> {
        self.archive
            .entries()
            .iter()
            .find(|e| e.name == name)
            .map(|e| e.declared_size)
    }

    /// Preflights the fixed declared size against a caller limit, not a Format 1 rule.
    /// Accepted reads still perform full verification, exactly as [`Self::read`].
    pub fn read_limited(&mut self, name: &str, maximum: u64) -> Result<Vec<u8>, PackageError> {
        let index = self
            .archive
            .entries()
            .iter()
            .position(|e| e.name == name)
            .ok_or_else(|| PackageError::EntryNotFound(name.to_owned()))?;
        let entry = &self.archive.entries()[index];
        if entry.declared_size > maximum {
            return Err(PackageError::ReadSizeLimit(maximum).at_entry(&entry.name));
        }
        self.read_index(index)
    }

    fn read_index(&mut self, index: usize) -> Result<Vec<u8>, PackageError> {
        let bytes = self.archive.read_entry(index)?;
        let name = &self.archive.entries()[index].name;
        if let Some(expected) = self.metadata.get(name) {
            if bytes != *expected {
                return Err(PackageError::MetadataChanged.at_entry(name));
            }
        } else if let Some(hashes) = &self.hashes {
            let expected = hashes
                .get(name)
                .ok_or_else(|| PackageError::MissingHash(name.clone()))?;
            list::verify_hash(&bytes, expected).map_err(|e| e.at_entry(name))?;
        }
        Ok(bytes)
    }
}

impl<T: AsRef<[u8]>> Package<Cursor<T>> {
    pub fn from_bytes(bytes: T) -> Result<Self, PackageError> {
        Self::from_reader(Cursor::new(bytes))
    }
}

impl Package<Cursor<Vec<u8>>> {
    /// Opens one regular-file handle and fully verifies an owned archive snapshot.
    /// `max_bytes` bounds compressed bytes; Format 1 limits still apply. Later path
    /// replacement or file writes cannot change this package. Observed length changes
    /// during acquisition are rejected; concurrent same-size writes are not atomic.
    pub fn open_snapshot(path: impl AsRef<Path>, max_bytes: u64) -> Result<Self, PackageError> {
        Self::from_bytes(snapshot::open(path.as_ref(), max_bytes)?)
    }
}

impl Package<File> {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, PackageError> {
        Self::from_reader(File::open(path)?)
    }
}
