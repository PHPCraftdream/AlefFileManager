// SPDX-License-Identifier: MIT OR Apache-2.0
//! `crypto`: what an application needs and its page cannot do for itself. The WebCrypto of the engine is
//! not in the pages of an application (the feature is off and the origin is not a secure context, see
//! `docs/stages/m3-data.md`), and even `crypto.getRandomValues` is missing, so the program gives them:
//! random bytes, digests, HMAC, HKDF, PBKDF2, the password hashes Argon2id and scrypt, authenticated
//! encryption (AES-GCM, ChaCha20-Poly1305) and Ed25519 signatures. Nothing here touches the machine, so
//! no right is asked for. Data goes as the body of a call, keys and salts as base64 in its arguments,
//! and what comes back is bytes.
use std::num::NonZeroU32;

use alef_core::{
    registry::{command::Reply, context::CallContext, dispatch::Registry},
    AlefError, ErrorCode,
};
use base64::{engine::general_purpose::STANDARD, Engine};
use bytes::Bytes;
use ring::{
    aead, digest, hkdf, hmac, pbkdf2,
    rand::{SecureRandom, SystemRandom},
    signature::{self, Ed25519KeyPair, KeyPair},
};
use serde::Deserialize;
use serde_json::json;
use tokio::sync::Semaphore;

use crate::json;

/// The most `crypto.random` gives at once.
pub const MAX_RANDOM: usize = 1024 * 1024;
/// The most a derived key or a hash is long.
pub const MAX_DERIVED: usize = 1024;
/// The most iterations of PBKDF2.
pub const MAX_PBKDF2_ITERATIONS: u32 = 10_000_000;
/// The most memory a password hash may ask for (Argon2id: KiB; scrypt: `128 * N * r` bytes).
pub const MAX_MEMORY: u64 = 1024 * 1024 * 1024;
const NONCE: usize = 12;

/// The password hashes eat memory and time: only so many run at once, and the others wait.
static HEAVY: Semaphore = Semaphore::const_new(2);

fn invalid(message: &str) -> AlefError {
    AlefError::new(ErrorCode::InvalidArgument, message)
}

fn unbase64(what: &str, text: &str) -> Result<Vec<u8>, AlefError> {
    STANDARD
        .decode(text)
        .map_err(|_| invalid(&format!("{what} is base64")))
}

async fn blocking<T: Send + 'static>(
    work: impl FnOnce() -> Result<T, AlefError> + Send + 'static,
) -> Result<T, AlefError> {
    tokio::task::spawn_blocking(work)
        .await
        .map_err(|error| AlefError::new(ErrorCode::Internal, error.to_string()))?
}

#[derive(Clone, Copy)]
enum Hash {
    Sha1,
    Sha256,
    Sha384,
    Sha512,
}

impl Hash {
    fn parse(name: &str) -> Result<Self, AlefError> {
        match name {
            "sha-1" => Ok(Self::Sha1),
            "sha-256" => Ok(Self::Sha256),
            "sha-384" => Ok(Self::Sha384),
            "sha-512" => Ok(Self::Sha512),
            _ => Err(invalid("a hash is sha-1, sha-256, sha-384 or sha-512")),
        }
    }

    fn digest(self) -> &'static digest::Algorithm {
        match self {
            Self::Sha1 => &digest::SHA1_FOR_LEGACY_USE_ONLY,
            Self::Sha256 => &digest::SHA256,
            Self::Sha384 => &digest::SHA384,
            Self::Sha512 => &digest::SHA512,
        }
    }

    fn hmac(self) -> hmac::Algorithm {
        match self {
            Self::Sha1 => hmac::HMAC_SHA1_FOR_LEGACY_USE_ONLY,
            Self::Sha256 => hmac::HMAC_SHA256,
            Self::Sha384 => hmac::HMAC_SHA384,
            Self::Sha512 => hmac::HMAC_SHA512,
        }
    }

    fn hkdf(self) -> hkdf::Algorithm {
        match self {
            Self::Sha1 => hkdf::HKDF_SHA1_FOR_LEGACY_USE_ONLY,
            Self::Sha256 => hkdf::HKDF_SHA256,
            Self::Sha384 => hkdf::HKDF_SHA384,
            Self::Sha512 => hkdf::HKDF_SHA512,
        }
    }

    fn pbkdf2(self) -> pbkdf2::Algorithm {
        match self {
            Self::Sha1 => pbkdf2::PBKDF2_HMAC_SHA1,
            Self::Sha256 => pbkdf2::PBKDF2_HMAC_SHA256,
            Self::Sha384 => pbkdf2::PBKDF2_HMAC_SHA384,
            Self::Sha512 => pbkdf2::PBKDF2_HMAC_SHA512,
        }
    }
}

fn derived_length(length: usize) -> Result<usize, AlefError> {
    if length == 0 || length > MAX_DERIVED {
        return Err(invalid("a derived key is from 1 to 1024 bytes"));
    }
    Ok(length)
}

fn pbkdf2_iterations(count: u32) -> Result<NonZeroU32, AlefError> {
    NonZeroU32::new(count)
        .filter(|count| count.get() <= MAX_PBKDF2_ITERATIONS)
        .ok_or_else(|| invalid("iterations are from 1 to 10000000"))
}

/// The cost of Argon2id and the length of the hash: the advice of OWASP (19 MiB, 2 passes, 1 lane,
/// 32 bytes) unless told otherwise.
fn argon2_setup(args: &Argon2Args) -> Result<(argon2::Params, usize), AlefError> {
    let memory = args.memory_kib.unwrap_or(19 * 1024);
    let iterations = args.iterations.unwrap_or(2);
    let parallelism = args.parallelism.unwrap_or(1);
    let length = args.length.unwrap_or(32);
    if u64::from(memory) * 1024 > MAX_MEMORY {
        return Err(invalid("Argon2id may ask for up to 1 GiB of memory"));
    }
    if !(1..=100).contains(&iterations) || !(1..=16).contains(&parallelism) {
        return Err(invalid("Argon2id: 1 to 100 iterations, 1 to 16 lanes"));
    }
    if !(4..=MAX_DERIVED).contains(&length) {
        return Err(invalid("a hash of Argon2id is from 4 to 1024 bytes"));
    }
    let params = argon2::Params::new(memory, iterations, parallelism, Some(length))
        .map_err(|_| invalid("these parameters of Argon2id do not go together"))?;
    Ok((params, length))
}

/// The cost of scrypt and the length of the hash: the advice of OWASP (N = 2^17, r = 8, p = 1, 32
/// bytes) unless told otherwise.
fn scrypt_setup(args: &ScryptArgs) -> Result<(scrypt::Params, usize), AlefError> {
    let log_n = args.log_n.unwrap_or(17);
    let r = args.r.unwrap_or(8);
    let p = args.p.unwrap_or(1);
    let length = args.length.unwrap_or(32);
    if !(1..=24).contains(&log_n) || !(1..=64).contains(&r) || !(1..=16).contains(&p) {
        return Err(invalid("scrypt: logN 1 to 24, r 1 to 64, p 1 to 16"));
    }
    if 128 * (1_u64 << log_n) * u64::from(r) > MAX_MEMORY {
        return Err(invalid("scrypt may ask for up to 1 GiB of memory"));
    }
    let length = derived_length(length)?;
    // The length of the key is that of the output: `Params` would take only 10 to 64 for it.
    let params = scrypt::Params::new(log_n, r, p, scrypt::Params::RECOMMENDED_LEN)
        .map_err(|_| invalid("these parameters of scrypt do not go together"))?;
    Ok((params, length))
}

#[derive(Clone, Copy)]
enum Cipher {
    Aes128Gcm,
    Aes256Gcm,
    ChaCha20Poly1305,
}

impl Cipher {
    fn parse(name: &str) -> Result<Self, AlefError> {
        match name {
            "aes-128-gcm" => Ok(Self::Aes128Gcm),
            "aes-256-gcm" => Ok(Self::Aes256Gcm),
            "chacha20-poly1305" => Ok(Self::ChaCha20Poly1305),
            _ => Err(invalid(
                "a cipher is aes-128-gcm, aes-256-gcm or chacha20-poly1305",
            )),
        }
    }

    fn algorithm(self) -> &'static aead::Algorithm {
        match self {
            Self::Aes128Gcm => &aead::AES_128_GCM,
            Self::Aes256Gcm => &aead::AES_256_GCM,
            Self::ChaCha20Poly1305 => &aead::CHACHA20_POLY1305,
        }
    }

    fn key(self, text: &str) -> Result<aead::LessSafeKey, AlefError> {
        let bytes = unbase64("the key", text)?;
        let key = aead::UnboundKey::new(self.algorithm(), &bytes)
            .map_err(|_| invalid("the key is not the length of this cipher (16 or 32 bytes)"))?;
        Ok(aead::LessSafeKey::new(key))
    }
}

fn random(length: usize) -> Result<Vec<u8>, AlefError> {
    let mut bytes = vec![0; length];
    SystemRandom::new()
        .fill(&mut bytes)
        .map_err(|_| AlefError::new(ErrorCode::Internal, "no random bytes"))?;
    Ok(bytes)
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct RandomArgs {
    length: usize,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct DigestArgs {
    algorithm: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HmacArgs {
    algorithm: String,
    key: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HmacVerifyArgs {
    algorithm: String,
    key: String,
    tag: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct HkdfArgs {
    algorithm: String,
    #[serde(default)]
    salt: Option<String>,
    #[serde(default)]
    info: Option<String>,
    length: usize,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct Pbkdf2Args {
    algorithm: String,
    salt: String,
    iterations: u32,
    length: usize,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct Argon2Args {
    salt: String,
    #[serde(default, rename = "memoryKiB")]
    memory_kib: Option<u32>,
    #[serde(default)]
    iterations: Option<u32>,
    #[serde(default)]
    parallelism: Option<u32>,
    #[serde(default)]
    length: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct ScryptArgs {
    salt: String,
    #[serde(default)]
    log_n: Option<u8>,
    #[serde(default)]
    r: Option<u32>,
    #[serde(default)]
    p: Option<u32>,
    #[serde(default)]
    length: Option<usize>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
struct SealArgs {
    algorithm: String,
    key: String,
    #[serde(default)]
    aad: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct SignArgs {
    private_key: String,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields, rename_all = "camelCase")]
struct VerifyArgs {
    public_key: String,
    signature: String,
}

fn body(ctx: &CallContext) -> Bytes {
    ctx.body().cloned().unwrap_or_default()
}

fn bytes(data: Vec<u8>) -> Result<Reply, AlefError> {
    Ok(Reply::Bytes(Bytes::from(data)))
}

/// The key pair of Ed25519 that a 32-byte seed makes.
fn pair(private_key: &str) -> Result<Ed25519KeyPair, AlefError> {
    let seed = unbase64("the private key", private_key)?;
    Ed25519KeyPair::from_seed_unchecked(&seed)
        .map_err(|_| invalid("the private key of Ed25519 is a seed of 32 bytes"))
}

struct Length(usize);

impl hkdf::KeyType for Length {
    fn len(&self) -> usize {
        self.0
    }
}

pub(crate) fn register(registry: &mut Registry) -> Result<(), AlefError> {
    registry
        .command::<RandomArgs>("crypto.random")?
        .handler(|_ctx, args| async move {
            if args.length > MAX_RANDOM {
                return Err(invalid("up to 1 MiB of random bytes at a time"));
            }
            bytes(blocking(move || random(args.length)).await?)
        })?;

    registry
        .command::<DigestArgs>("crypto.digest")?
        .handler(|ctx, args| async move {
            let hash = Hash::parse(&args.algorithm)?;
            let data = body(&ctx);
            bytes(
                blocking(move || Ok(digest::digest(hash.digest(), &data).as_ref().to_vec()))
                    .await?,
            )
        })?;

    registry
        .command::<HmacArgs>("crypto.hmac")?
        .handler(|ctx, args| async move {
            let hash = Hash::parse(&args.algorithm)?;
            let key = hmac::Key::new(hash.hmac(), &unbase64("the key", &args.key)?);
            let data = body(&ctx);
            bytes(blocking(move || Ok(hmac::sign(&key, &data).as_ref().to_vec())).await?)
        })?;

    registry
        .command::<HmacVerifyArgs>("crypto.hmacVerify")?
        .handler(|ctx, args| async move {
            let hash = Hash::parse(&args.algorithm)?;
            let key = hmac::Key::new(hash.hmac(), &unbase64("the key", &args.key)?);
            let tag = unbase64("the tag", &args.tag)?;
            let data = body(&ctx);
            let valid = blocking(move || Ok(hmac::verify(&key, &data, &tag).is_ok())).await?;
            json(&json!({ "valid": valid }))
        })?;

    registry
        .command::<HkdfArgs>("crypto.hkdf")?
        .handler(|ctx, args| async move {
            let hash = Hash::parse(&args.algorithm)?;
            let length = derived_length(args.length)?;
            let salt = args
                .salt
                .map(|text| unbase64("the salt", &text))
                .transpose()?
                .unwrap_or_default();
            let info = args
                .info
                .map(|text| unbase64("the info", &text))
                .transpose()?
                .unwrap_or_default();
            let ikm = body(&ctx);
            bytes(
                blocking(move || {
                    let prk = hkdf::Salt::new(hash.hkdf(), &salt).extract(&ikm);
                    let mut out = vec![0; length];
                    prk.expand(&[info.as_slice()], Length(length))
                        .and_then(|okm| okm.fill(&mut out))
                        .map_err(|_| invalid("HKDF cannot give so much"))?;
                    Ok(out)
                })
                .await?,
            )
        })?;

    registry
        .command::<Pbkdf2Args>("crypto.pbkdf2")?
        .handler(|ctx, args| async move {
            let hash = Hash::parse(&args.algorithm)?;
            let length = derived_length(args.length)?;
            let iterations = pbkdf2_iterations(args.iterations)?;
            let salt = unbase64("the salt", &args.salt)?;
            let password = body(&ctx);
            let _turn = HEAVY.acquire().await;
            bytes(
                blocking(move || {
                    let mut out = vec![0; length];
                    pbkdf2::derive(hash.pbkdf2(), iterations, &salt, &password, &mut out);
                    Ok(out)
                })
                .await?,
            )
        })?;

    registry
        .command::<Argon2Args>("crypto.argon2id")?
        .handler(|ctx, args| async move {
            let (params, length) = argon2_setup(&args)?;
            let salt = unbase64("the salt", &args.salt)?;
            if salt.len() < 8 {
                return Err(invalid("the salt of Argon2id is at least 8 bytes"));
            }
            let password = body(&ctx);
            let _turn = HEAVY.acquire().await;
            bytes(
                blocking(move || {
                    let mut out = vec![0; length];
                    argon2::Argon2::new(
                        argon2::Algorithm::Argon2id,
                        argon2::Version::V0x13,
                        params,
                    )
                    .hash_password_into(&password, &salt, &mut out)
                    .map_err(|_| invalid("Argon2id cannot hash with these arguments"))?;
                    Ok(out)
                })
                .await?,
            )
        })?;

    registry
        .command::<ScryptArgs>("crypto.scrypt")?
        .handler(|ctx, args| async move {
            let (params, length) = scrypt_setup(&args)?;
            let salt = unbase64("the salt", &args.salt)?;
            let password = body(&ctx);
            let _turn = HEAVY.acquire().await;
            bytes(
                blocking(move || {
                    let mut out = vec![0; length];
                    scrypt::scrypt(&password, &salt, &params, &mut out)
                        .map_err(|_| invalid("scrypt cannot hash with these arguments"))?;
                    Ok(out)
                })
                .await?,
            )
        })?;

    registry
        .command::<SealArgs>("crypto.seal")?
        .handler(|ctx, args| async move {
            let cipher = Cipher::parse(&args.algorithm)?;
            let key = cipher.key(&args.key)?;
            let aad = args
                .aad
                .map(|text| unbase64("the aad", &text))
                .transpose()?
                .unwrap_or_default();
            let plain = body(&ctx);
            bytes(
                blocking(move || {
                    let nonce = random(NONCE)?;
                    let mut sealed = plain.to_vec();
                    key.seal_in_place_append_tag(
                        aead::Nonce::try_assume_unique_for_key(&nonce)
                            .map_err(|_| AlefError::new(ErrorCode::Internal, "no nonce"))?,
                        aead::Aad::from(aad),
                        &mut sealed,
                    )
                    .map_err(|_| invalid("this message cannot be sealed"))?;
                    // The nonce goes first: nobody has to keep it apart.
                    let mut out = nonce;
                    out.append(&mut sealed);
                    Ok(out)
                })
                .await?,
            )
        })?;

    registry
        .command::<SealArgs>("crypto.open")?
        .handler(|ctx, args| async move {
            let cipher = Cipher::parse(&args.algorithm)?;
            let key = cipher.key(&args.key)?;
            let aad = args
                .aad
                .map(|text| unbase64("the aad", &text))
                .transpose()?
                .unwrap_or_default();
            let sealed = body(&ctx);
            if sealed.len() < NONCE + cipher.algorithm().tag_len() {
                return Err(invalid("a sealed message is a nonce, the text and a tag"));
            }
            bytes(
                blocking(move || {
                    let (nonce, rest) = sealed.split_at(NONCE);
                    let mut text = rest.to_vec();
                    let opened = key
                        .open_in_place(
                            aead::Nonce::try_assume_unique_for_key(nonce)
                                .map_err(|_| invalid("a sealed message starts with a nonce"))?,
                            aead::Aad::from(aad),
                            &mut text,
                        )
                        .map_err(|_| invalid("the message did not pass authentication"))?;
                    Ok(opened.to_vec())
                })
                .await?,
            )
        })?;

    registry
        .command::<()>("crypto.ed25519Generate")?
        .handler(|_ctx, ()| async move {
            let (seed, public) = blocking(|| {
                let seed = random(32)?;
                let pair = Ed25519KeyPair::from_seed_unchecked(&seed)
                    .map_err(|_| AlefError::new(ErrorCode::Internal, "no key pair"))?;
                Ok((seed, pair.public_key().as_ref().to_vec()))
            })
            .await?;
            json(&json!({
                "privateKey": STANDARD.encode(seed),
                "publicKey": STANDARD.encode(public),
            }))
        })?;

    registry
        .command::<SignArgs>("crypto.ed25519Sign")?
        .handler(|ctx, args| async move {
            let pair = pair(&args.private_key)?;
            let message = body(&ctx);
            bytes(blocking(move || Ok(pair.sign(&message).as_ref().to_vec())).await?)
        })?;

    registry
        .command::<VerifyArgs>("crypto.ed25519Verify")?
        .handler(|ctx, args| async move {
            let public = unbase64("the public key", &args.public_key)?;
            let signature = unbase64("the signature", &args.signature)?;
            let message = body(&ctx);
            let valid = blocking(move || {
                Ok(
                    signature::UnparsedPublicKey::new(&signature::ED25519, &public)
                        .verify(&message, &signature)
                        .is_ok(),
                )
            })
            .await?;
            json(&json!({ "valid": valid }))
        })
}

#[cfg(test)]
mod tests {
    use serde_json::{json, Value};

    use super::*;

    fn argon2(args: Value) -> Result<(argon2::Params, usize), AlefError> {
        let mut all = json!({ "salt": "" });
        all.as_object_mut()
            .unwrap()
            .extend(args.as_object().unwrap().clone());
        argon2_setup(&serde_json::from_value(all).unwrap())
    }

    fn scrypt(args: Value) -> Result<(scrypt::Params, usize), AlefError> {
        let mut all = json!({ "salt": "" });
        all.as_object_mut()
            .unwrap()
            .extend(args.as_object().unwrap().clone());
        scrypt_setup(&serde_json::from_value(all).unwrap())
    }

    fn refused<T>(result: Result<T, AlefError>, what: &str) {
        match result {
            Err(error) => assert_eq!(error.code, ErrorCode::InvalidArgument, "{what}"),
            Ok(_) => panic!("{what} was let through"),
        }
    }

    #[test]
    fn pbkdf2_takes_from_1_to_10_million_iterations() {
        refused(pbkdf2_iterations(0), "none");
        assert!(pbkdf2_iterations(1).is_ok());
        assert!(pbkdf2_iterations(10_000_000).is_ok());
        refused(pbkdf2_iterations(10_000_001), "one more than the most");
    }

    #[test]
    fn argon2id_costs_what_owasp_advises_unless_told_otherwise() {
        let (params, length) = argon2(json!({})).unwrap();
        assert_eq!(
            (params.m_cost(), params.t_cost(), params.p_cost(), length),
            (19 * 1024, 2, 1, 32)
        );
    }

    #[test]
    fn the_limits_of_argon2id_are_exact() {
        let at = |memory: u32, passes: u32, lanes: u32, length: usize| {
            argon2(json!({
                "memoryKiB": memory, "iterations": passes, "parallelism": lanes, "length": length,
            }))
        };
        assert!(at(1024 * 1024, 1, 1, 32).is_ok(), "1 GiB");
        refused(at(1024 * 1024 + 1, 1, 1, 32), "1 GiB and a KiB");
        refused(at(8, 0, 1, 32), "no passes");
        assert!(at(8, 1, 1, 32).is_ok());
        assert!(at(8, 100, 1, 32).is_ok());
        refused(at(8, 101, 1, 32), "101 passes");
        refused(at(8, 1, 0, 32), "no lanes");
        assert!(at(128, 1, 16, 32).is_ok());
        refused(at(136, 1, 17, 32), "17 lanes");
        refused(at(8, 1, 1, 3), "a hash of 3 bytes");
        assert!(at(8, 1, 1, 4).is_ok());
        assert!(at(8, 1, 1, 1024).is_ok());
        refused(at(8, 1, 1, 1025), "a hash of 1025 bytes");
    }

    #[test]
    fn scrypt_costs_what_owasp_advises_unless_told_otherwise() {
        let (params, length) = scrypt(json!({})).unwrap();
        assert_eq!(
            (params.log_n(), params.r(), params.p(), length),
            (17, 8, 1, 32)
        );
    }

    #[test]
    fn the_limits_of_scrypt_are_exact() {
        let at = |log_n: u32, r: u32, p: u32, length: usize| {
            scrypt(json!({ "logN": log_n, "r": r, "p": p, "length": length }))
        };
        refused(at(0, 8, 1, 32), "no cost");
        assert!(at(1, 8, 1, 32).is_ok());
        // 1 GiB is 128 * 2^20 * 8.
        assert!(at(20, 8, 1, 32).is_ok(), "1 GiB");
        refused(at(20, 9, 1, 32), "1 GiB and a block more");
        refused(at(21, 8, 1, 32), "2 GiB");
        refused(at(25, 1, 1, 32), "a cost of 25");
        refused(at(10, 0, 1, 32), "no block");
        assert!(at(10, 64, 1, 32).is_ok());
        refused(at(10, 65, 1, 32), "a block of 65");
        refused(at(10, 8, 0, 32), "no lanes");
        assert!(at(10, 8, 16, 32).is_ok());
        refused(at(10, 8, 17, 32), "17 lanes");
        refused(at(10, 8, 1, 0), "a hash of no bytes");
        assert!(at(10, 8, 1, 1).is_ok());
        assert!(at(10, 8, 1, 1024).is_ok());
        refused(at(10, 8, 1, 1025), "a hash of 1025 bytes");
    }
}
