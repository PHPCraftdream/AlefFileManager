// SPDX-License-Identifier: MIT OR Apache-2.0
pub(crate) mod list;
pub(crate) mod signature;

/// Cryptographic identity only, not a claim of developer identity or trust.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Signer {
    Signed { key: [u8; 32], fingerprint: String },
    Unsigned,
}

#[cfg(test)]
mod tests;
