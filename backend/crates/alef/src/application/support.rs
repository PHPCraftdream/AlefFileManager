// SPDX-License-Identifier: MIT OR Apache-2.0
//! Builders of test packages and manifests, independent of the production helpers.

use base64::{engine::general_purpose::STANDARD, Engine};
use ed25519_dalek::{Signer as _, SigningKey};
use sha2::{Digest, Sha256};

pub fn hashes(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut files = files.to_vec();
    files.sort_by_key(|f| f.0);
    files
        .iter()
        .map(|(name, bytes)| format!("{:x}  {name}\n", Sha256::digest(bytes)))
        .collect::<String>()
        .into_bytes()
}

/// The signature record over `hashes` and the hex fingerprint of the fixed test key.
pub fn signature(hashes: &[u8]) -> (Vec<u8>, String) {
    let key = SigningKey::from_bytes(&[42; 32]);
    let public = *key.verifying_key().as_bytes();
    let message = [b"alef-package-v1\n".as_slice(), hashes].concat();
    let record = format!(
        "alef-package 1\nkey {}\nsig {}\n",
        STANDARD.encode(public),
        STANDARD.encode(key.sign(&message).to_bytes())
    )
    .into_bytes();
    (record, format!("{:x}", Sha256::digest(&public)))
}

/// The hex public key of the fixed test key (see [`signature`]).
pub fn test_key_hex() -> String {
    let key = SigningKey::from_bytes(&[42; 32]);
    key.verifying_key()
        .as_bytes()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

/// A package with a hash list and a valid signature; returns the archive and the fingerprint.
pub fn signed_package(files: &[(&str, &[u8])]) -> (Vec<u8>, String) {
    let list = hashes(files);
    let (sig, fingerprint) = signature(&list);
    let mut records = files.to_vec();
    records.push(("META-INF/hashes", &list));
    records.push(("META-INF/signature", &sig));
    (zip(&records), fingerprint)
}

#[allow(dead_code)]
pub fn unsigned_package(files: &[(&str, &[u8])]) -> Vec<u8> {
    zip(&files.to_vec())
}

#[allow(dead_code)]
pub fn hashed_package(files: &[(&str, &[u8])]) -> Vec<u8> {
    let list = hashes(files);
    let mut records = files.to_vec();
    records.push(("META-INF/hashes", &list));
    zip(&records)
}

fn put16(bytes: &mut [u8], at: usize, value: u16) {
    bytes[at..at + 2].copy_from_slice(&value.to_le_bytes());
}
fn put32(bytes: &mut [u8], at: usize, value: u32) {
    bytes[at..at + 4].copy_from_slice(&value.to_le_bytes());
}
// Independent stored-only canonical ZIP builder; no production parser helpers.
pub fn zip(files: &[(&str, &[u8])]) -> Vec<u8> {
    let mut files = files.to_vec();
    files.sort_by_key(|f| f.0);
    let mut bytes = Vec::new();
    let mut offsets = Vec::new();
    for (name, data) in &files {
        offsets.push(bytes.len() as u32);
        let mut h = [0; 30];
        h[..4].copy_from_slice(b"PK\x03\x04");
        put16(&mut h, 4, 20);
        put16(&mut h, 6, 0x0800);
        put16(&mut h, 8, 0);
        put16(&mut h, 12, 0x21);
        put32(&mut h, 14, crc32fast::hash(data));
        put32(&mut h, 18, data.len() as u32);
        put32(&mut h, 22, data.len() as u32);
        put16(&mut h, 26, name.len() as u16);
        bytes.extend(h);
        bytes.extend(name.as_bytes());
        bytes.extend_from_slice(data);
    }
    let start = bytes.len();
    for ((name, data), offset) in files.iter().zip(offsets) {
        let mut h = [0; 46];
        h[..4].copy_from_slice(b"PK\x01\x02");
        put16(&mut h, 4, 0x031e);
        put16(&mut h, 6, 20);
        put16(&mut h, 8, 0x0800);
        put16(&mut h, 10, 0);
        put16(&mut h, 14, 0x21);
        put32(&mut h, 16, crc32fast::hash(data));
        put32(&mut h, 20, data.len() as u32);
        put32(&mut h, 24, data.len() as u32);
        put16(&mut h, 28, name.len() as u16);
        put32(&mut h, 38, 0x81a40000);
        put32(&mut h, 42, offset);
        bytes.extend(h);
        bytes.extend(name.as_bytes());
    }
    let mut h = [0; 22];
    h[..4].copy_from_slice(b"PK\x05\x06");
    put16(&mut h, 8, files.len() as u16);
    put16(&mut h, 10, files.len() as u16);
    put32(&mut h, 12, (bytes.len() - start) as u32);
    put32(&mut h, 16, start as u32);
    bytes.extend(h);
    bytes
}

/// A minimal valid manifest; `extra` lines are spliced in after the version.
pub fn manifest_text(extra: &str) -> String {
    format!(
        "id: org.example.app
name: Example
version:: 1.0.0
{extra}windows: [
    {{
        label: main
        url: /index.html
        width: 800
        height: 600
    }}
]
external: {{
    connect: []
    load: {{
        scripts: []
        styles: []
        images: []
        fonts: []
        media: []
        frames: []
    }}
}}
permissions: {{
    fs: {{
        read: []
        write: []
    }}
    cli: {{
        exec: []
    }}
    net: {{
        http: []
        socket: []
    }}
    shell: {{
        openExternal: []
    }}
    clipboard: {{
        read: false
    }}
    shortcut: {{
        global: false
    }}
    secrets: false
    app: {{
        env: []
    }}
}}
"
    )
}

/// A minimal PNG: signature, IHDR with the given dimensions, empty IDAT and IEND.
pub fn png(width: u32, height: u32) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&[0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A]);
    let mut chunk = Vec::new();
    chunk.extend_from_slice(b"IHDR");
    chunk.extend_from_slice(&width.to_be_bytes());
    chunk.extend_from_slice(&height.to_be_bytes());
    chunk.extend_from_slice(&[8, 6, 0, 0, 0]);
    bytes.extend_from_slice(&(chunk.len() as u32).to_be_bytes());
    bytes.extend_from_slice(&chunk);
    bytes.extend_from_slice(&crc32fast::hash(&chunk).to_be_bytes());
    bytes.extend_from_slice(&0u32.to_be_bytes());
    bytes.extend_from_slice(b"IDAT");
    bytes.extend_from_slice(&crc32fast::hash(b"IDAT").to_be_bytes());
    bytes.extend_from_slice(&0u32.to_be_bytes());
    bytes.extend_from_slice(b"IEND");
    bytes.extend_from_slice(&crc32fast::hash(b"IEND").to_be_bytes());
    bytes
}
