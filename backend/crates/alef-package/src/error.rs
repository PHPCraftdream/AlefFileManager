// SPDX-License-Identifier: MIT OR Apache-2.0
use std::{error::Error, fmt, io};

#[derive(Debug)]
#[non_exhaustive]
pub enum PackageError {
    Io(io::Error),
    InvalidUtf8(String),
    InvalidName(String, &'static str),
    DuplicateName(String),
    PathConflict(String),
    MissingManifest,
    InvalidArchive(&'static str),
    UnsupportedMethod(String, u16),
    MultiDisk,
    Zip64,
    ArchiveSizeLimit,
    CentralSizeLimit,
    EntryCountLimit,
    EntrySizeLimit(String),
    MetadataSizeLimit(String, u64),
    TotalSizeLimit,
    ArithmeticOverflow,
    Allocation,
    InvalidDeflate,
    PayloadSizeMismatch,
    CrcMismatch,
    InvalidHashList(&'static str),
    MissingHash(String),
    ExtraHash(String),
    HashMismatch,
    MissingHashes,
    InvalidSignature(&'static str),
    InvalidSigningKey,
    SignatureMismatch,
    MetadataChanged,
    EntryNotFound(String),
    Entry(String, Box<PackageError>),
}

impl PackageError {
    pub(crate) fn at_entry(self, name: &str) -> Self {
        Self::Entry(name.to_owned(), Box::new(self))
    }
}
impl fmt::Display for PackageError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io(e) => write!(f, "package I/O: {e}"),
            Self::InvalidUtf8(raw) => write!(f, "entry name {raw} is not UTF-8"),
            Self::InvalidName(name, reason) => write!(f, "invalid entry name {name:?}: {reason}"),
            Self::DuplicateName(name) => {
                write!(f, "ASCII-case-insensitive duplicate entry name {name:?}")
            }
            Self::PathConflict(name) => write!(f, "file/path prefix conflict at {name:?}"),
            Self::MissingManifest => f.write_str("missing alef.ktav"),
            Self::InvalidArchive(reason) => write!(f, "invalid archive: {reason}"),
            Self::UnsupportedMethod(name, method) => {
                write!(f, "entry {name:?}: unsupported ZIP method: {method}")
            }
            Self::MultiDisk => f.write_str("multi-disk ZIP archives are forbidden"),
            Self::Zip64 => f.write_str("ZIP64 is not supported by format 1"),
            Self::ArchiveSizeLimit => f.write_str("archive exceeds 2304 MiB"),
            Self::CentralSizeLimit => f.write_str("central directory exceeds 16 MiB"),
            Self::EntryCountLimit => f.write_str("archive exceeds 10,000 entries"),
            Self::EntrySizeLimit(name) => write!(f, "entry {name:?} exceeds 512 MiB declared size"),
            Self::MetadataSizeLimit(name, limit) => {
                write!(f, "entry {name:?} exceeds {limit} bytes")
            }
            Self::TotalSizeLimit => f.write_str("archive exceeds 2 GiB declared total size"),
            Self::ArithmeticOverflow => f.write_str("archive metadata arithmetic overflow"),
            Self::Allocation => f.write_str("package buffer allocation failed"),
            Self::InvalidDeflate => f.write_str("invalid or truncated raw deflate stream"),
            Self::PayloadSizeMismatch => {
                f.write_str("payload does not consume/produce exactly the declared sizes")
            }
            Self::CrcMismatch => f.write_str("payload CRC-32 mismatch"),
            Self::InvalidHashList(reason) => write!(f, "invalid META-INF/hashes: {reason}"),
            Self::MissingHash(name) => write!(f, "missing hash for {name:?}"),
            Self::ExtraHash(name) => write!(f, "hash for non-payload entry {name:?}"),
            Self::HashMismatch => f.write_str("payload SHA-256 mismatch"),
            Self::MissingHashes => f.write_str("signature requires META-INF/hashes"),
            Self::InvalidSignature(reason) => {
                write!(f, "invalid META-INF/signature record: {reason}")
            }
            Self::InvalidSigningKey => {
                f.write_str("META-INF/signature: invalid Ed25519 signing key")
            }
            Self::SignatureMismatch => {
                f.write_str("META-INF/signature: strict Ed25519 verification failed")
            }
            Self::MetadataChanged => f.write_str("metadata differs from opening snapshot"),
            Self::EntryNotFound(name) => write!(f, "entry not found: {name:?}"),
            Self::Entry(name, error) => write!(f, "entry {name:?}: {error}"),
        }
    }
}
impl Error for PackageError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            Self::Entry(_, e) => Some(e.as_ref()),
            _ => None,
        }
    }
}
impl From<io::Error> for PackageError {
    fn from(error: io::Error) -> Self {
        Self::Io(error)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn metadata_errors_name_the_responsible_metadata_entry() {
        assert!(PackageError::InvalidHashList("bad line")
            .to_string()
            .contains("META-INF/hashes"));
        for error in [
            PackageError::InvalidSignature("bad record"),
            PackageError::InvalidSigningKey,
            PackageError::SignatureMismatch,
        ] {
            assert!(error.to_string().contains("META-INF/signature"));
        }
    }
}
