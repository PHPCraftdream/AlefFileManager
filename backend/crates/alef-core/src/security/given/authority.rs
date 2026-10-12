// SPDX-License-Identifier: MIT OR Apache-2.0
//! Host-selected application authority, separate from the public manifest id.
use sha2::{Digest, Sha256};

/// Established by the launcher/embedder, never by application-provided arguments.
/// This descriptor does not verify signatures or establish trust on its own.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub enum Authority {
    /// Development directory: retain every legacy storage namespace.
    #[default]
    Directory,
    /// Verified package signer fingerprint (not a verified developer name).
    SignedPackage { fingerprint: String },
    /// SHA-256 digest of the immutable unsigned archive bytes.
    UnsignedPackage { archive_digest: String },
}

impl Authority {
    /// Stable persistence token, or `None` for legacy directory authority.
    ///
    /// SHA-256 input is the fixed bytes `alef-package-authority-v1\0`, followed by
    /// three length-prefixed UTF-8 fields: kind (`signed` or `unsigned`), exact
    /// manifest id, exact fingerprint/archive digest. Each prefix is the byte
    /// length as an unsigned 64-bit big-endian integer. No case folding,
    /// normalization, delimiter escaping, or digest decoding is performed.
    /// Output is exactly 64 lowercase hexadecimal characters. This is namespace
    /// isolation, not authentication; callers must establish the authority first.
    pub fn package_token(&self, app_id: &str) -> Option<String> {
        match self {
            Self::Directory => None,
            Self::SignedPackage { fingerprint } => {
                Some(package_token("signed", app_id, fingerprint))
            }
            Self::UnsignedPackage { archive_digest } => {
                Some(package_token("unsigned", app_id, archive_digest))
            }
        }
    }
}

pub(crate) fn package_token(kind: &str, app_id: &str, identity: &str) -> String {
    let mut hash = Sha256::new();
    hash.update(b"alef-package-authority-v1\0");
    for field in [kind, app_id, identity] {
        // Supported Rust targets have at most 64-bit usize; no length truncation.
        hash.update((field.len() as u64).to_be_bytes());
        hash.update(field.as_bytes());
    }
    format!("{:x}", hash.finalize())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn independent_node_sha256_vectors() {
        // Computed once with node:crypto createHash('sha256'), Buffer UTF-8,
        // Buffer.writeBigUInt64BE(byteLength); expected values are not Rust-derived.
        let digest = "0123456789abcdef0123456789abcdef0123456789abcdef0123456789abcdef";
        for (authority, id, expected) in [
            (
                Authority::SignedPackage {
                    fingerprint: digest.into(),
                },
                "org.example.app",
                "20fd524915d71e1a1bda8a99c6af7715ab7f3931655bfa918d35d7b56a5f8343",
            ),
            (
                Authority::UnsignedPackage {
                    archive_digest: digest.into(),
                },
                "org.example.app",
                "9d7e2473837df35661dc156ca7948bc68d20cd613e43d11be365318dcd321c5a",
            ),
            (
                Authority::SignedPackage {
                    fingerprint: "c".into(),
                },
                "ab",
                "f82d36847be4f4483f2caa468121740b97e9e3a6c74891f2e0c44b4a3e811311",
            ),
            (
                Authority::SignedPackage {
                    fingerprint: "bc".into(),
                },
                "a",
                "ea1853f1d6847319dbf60551fdbbc7a93da1f5ff18b03c0877bb1b2199bb72dc",
            ),
            (
                Authority::SignedPackage {
                    fingerprint: "Ff:é\0".into(),
                },
                "é\0:id",
                "cc50edb615943d39fc517a58131c2d8209aa43aa4c1b201fd9d03c8f321e9c0a",
            ),
        ] {
            assert_eq!(authority.package_token(id).as_deref(), Some(expected));
        }
        assert_eq!(Authority::default().package_token("org.example.app"), None);
    }

    #[test]
    fn exact_fields_and_kinds_are_isolated() {
        let unsigned = |digest: &str| Authority::UnsignedPackage {
            archive_digest: digest.into(),
        };
        assert_ne!(
            unsigned("bc").package_token("a"),
            unsigned("c").package_token("ab")
        );
        let base = unsigned("abc").package_token("org.example.app");
        for other in [
            unsigned("abc").package_token("org.example.other"),
            unsigned("ABC").package_token("org.example.app"),
            unsigned("abc").package_token("Org.example.app"),
            Authority::SignedPackage {
                fingerprint: "abc".into(),
            }
            .package_token("org.example.app"),
            unsigned("a:b").package_token("c"),
        ] {
            assert_ne!(base, other);
        }
        assert_ne!(
            unsigned("b:c").package_token("a"),
            unsigned("c").package_token("a:b")
        );
        assert_ne!(
            unsigned("é").package_token("a"),
            unsigned("e\u{301}").package_token("a")
        );
    }
}
