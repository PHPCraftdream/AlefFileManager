// SPDX-License-Identifier: MIT OR Apache-2.0
use super::{
    list::{digest, hex},
    Signer,
};
use crate::{archive::MAX_SIGNATURE_SIZE, PackageError};
use base64::{engine::general_purpose::STANDARD, Engine};
use ed25519_dalek::{Signature, VerifyingKey};

pub(crate) const DOMAIN: &[u8] = b"alef-package-v1\n";

fn decode<const N: usize>(text: &str) -> Result<[u8; N], PackageError> {
    // STANDARD requires the padding and zero trailing bits, so a decode is canonical.
    STANDARD
        .decode(text)
        .map_err(|_| PackageError::InvalidSignature("base64"))?
        .try_into()
        .map_err(|_| PackageError::InvalidSignature("decoded length"))
}

pub(crate) fn verify(bytes: &[u8], hashes: &[u8]) -> Result<Signer, PackageError> {
    let invalid = PackageError::InvalidSignature;
    if bytes.len() as u64 > MAX_SIGNATURE_SIZE {
        return Err(invalid("size limit"));
    }
    let text = std::str::from_utf8(bytes).map_err(|_| invalid("UTF-8"))?;
    let mut lines = text.split('\n');
    if lines.next() != Some("alef-package 1") {
        return Err(invalid("version line"));
    }
    let key_text = lines
        .next()
        .and_then(|s| s.strip_prefix("key "))
        .ok_or(invalid("key line"))?;
    let sig_text = lines
        .next()
        .and_then(|s| s.strip_prefix("sig "))
        .ok_or(invalid("sig line"))?;
    if lines.next() != Some("") || lines.next().is_some() {
        return Err(invalid("exactly three LF-terminated lines required"));
    }
    let key = decode::<32>(key_text)?;
    let sig = Signature::from_bytes(&decode::<64>(sig_text)?);
    let verifier = VerifyingKey::from_bytes(&key).map_err(|_| PackageError::InvalidSigningKey)?;
    if verifier.is_weak() {
        return Err(PackageError::InvalidSigningKey);
    }
    let mut message = Vec::with_capacity(DOMAIN.len() + hashes.len());
    message.extend_from_slice(DOMAIN);
    message.extend_from_slice(hashes);
    verifier
        .verify_strict(&message, &sig)
        .map_err(|_| PackageError::SignatureMismatch)?;
    Ok(Signer::Signed {
        key,
        fingerprint: hex(&digest(&key)),
    })
}
