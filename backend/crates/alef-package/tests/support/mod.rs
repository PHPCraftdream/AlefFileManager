// SPDX-License-Identifier: MIT OR Apache-2.0
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
pub fn signature(hashes: &[u8]) -> Vec<u8> {
    let key = SigningKey::from_bytes(&[42; 32]);
    let message = [b"alef-package-v1\n".as_slice(), hashes].concat();
    format!(
        "alef-package 1\nkey {}\nsig {}\n",
        STANDARD.encode(key.verifying_key().as_bytes()),
        STANDARD.encode(key.sign(&message).to_bytes())
    )
    .into_bytes()
}
pub fn package(files: &[(&str, &[u8])], with_hashes: bool, signed: bool) -> Vec<u8> {
    let list = hashes(files);
    let sig = signature(&list);
    let mut records = files.to_vec();
    if with_hashes {
        records.push(("META-INF/hashes", &list));
    }
    if signed {
        records.push(("META-INF/signature", &sig));
    }
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
    zip_with_method(files, false)
}
pub fn deflated_zip(files: &[(&str, &[u8])]) -> Vec<u8> {
    zip_with_method(files, true)
}
fn zip_with_method(files: &[(&str, &[u8])], deflate: bool) -> Vec<u8> {
    use std::io::Write;
    let mut files = files.to_vec();
    files.sort_by_key(|f| f.0);
    let payloads: Vec<Vec<u8>> = files
        .iter()
        .map(|(_, data)| {
            if !deflate {
                return data.to_vec();
            }
            let mut encoder =
                flate2::write::DeflateEncoder::new(Vec::new(), flate2::Compression::default());
            encoder.write_all(data).unwrap();
            encoder.finish().unwrap()
        })
        .collect();
    let mut bytes = Vec::new();
    let mut offsets = Vec::new();
    for ((name, data), payload) in files.iter().zip(&payloads) {
        offsets.push(bytes.len() as u32);
        let mut h = [0; 30];
        h[..4].copy_from_slice(b"PK\x03\x04");
        put16(&mut h, 4, 20);
        put16(&mut h, 6, 0x0800);
        put16(&mut h, 8, if deflate { 8 } else { 0 });
        put16(&mut h, 12, 0x21);
        put32(&mut h, 14, crc32fast::hash(data));
        put32(&mut h, 18, payload.len() as u32);
        put32(&mut h, 22, data.len() as u32);
        put16(&mut h, 26, name.len() as u16);
        bytes.extend(h);
        bytes.extend(name.as_bytes());
        bytes.extend(payload);
    }
    let start = bytes.len();
    for (((name, data), offset), payload) in files.iter().zip(offsets).zip(&payloads) {
        let mut h = [0; 46];
        h[..4].copy_from_slice(b"PK\x01\x02");
        put16(&mut h, 4, 0x031e);
        put16(&mut h, 6, 20);
        put16(&mut h, 8, 0x0800);
        put16(&mut h, 10, if deflate { 8 } else { 0 });
        put16(&mut h, 14, 0x21);
        put32(&mut h, 16, crc32fast::hash(data));
        put32(&mut h, 20, payload.len() as u32);
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
