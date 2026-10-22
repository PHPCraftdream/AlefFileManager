// SPDX-License-Identifier: MIT OR Apache-2.0
//! Mutual token authentication handshake for the warm-start broker, pure logic.
//!
//! The challenge token is stored in a protected file; storage itself is the
//! transport/`ipc` layer's job, never here. The token is NOT passed via CLI,
//! environment, or logs — [`Token`] has no `Debug`/`Display` impls so it cannot
//! be formatted accidentally.
//!
//! Handshake message order (both sides must complete it):
//!
//! 1. server -> client: `nonce_s`
//! 2. client -> server: `nonce_c`, `mac_client = HMAC(token, "client" || nonce_s || nonce_c)`
//! 3. server verifies `mac_client` (constant time), then
//!    server -> client: `mac_server = HMAC(token, "server" || nonce_c || nonce_s)`
//! 4. client verifies `mac_server` (constant time).
//!
//! Both directions are verified, never just one. Nonces MUST be fresh per
//! connection; freshness defeats replay and substitution. Nonce generation is
//! injected via [`NonceSource`]; tests use deterministic sources.
//!
//! HMAC-SHA256 is implemented here per RFC 2104 on top of `sha2` primitives
//! (no `hmac` dependency) and verified against RFC 4231 test vectors.

use sha2::{Digest, Sha256};

/// Nonce length in bytes.
pub const NONCE_LEN: usize = 32;
/// Byte-array nonce.
pub type NonceBytes = [u8; NONCE_LEN];

/// Source of per-connection nonces. Must yield fresh values per connection.
pub trait NonceSource {
    /// Generate one nonce.
    fn generate(&mut self) -> NonceBytes;
}

/// Real [`NonceSource`] backed by the OS entropy source.
pub struct RandomNonce;

impl NonceSource for RandomNonce {
    /// Panics on OS entropy failure: unrecoverable, fail loudly rather than
    /// degrade to a predictable nonce.
    fn generate(&mut self) -> NonceBytes {
        let mut n = [0u8; NONCE_LEN];
        getrandom::fill(&mut n).expect("nonce generation failed");
        n
    }
}

/// Shared authentication secret.
///
/// Deliberately no `Debug`, `Display`, `Clone`, `PartialEq`: the secret must
/// never be logged, copied around, or compared non-constant-time. Zeroized on
/// drop.
pub struct Token(Vec<u8>);

/// Errors from [`Token::new`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TokenError {
    /// Token must be non-empty.
    Empty,
}

impl Token {
    /// Wrap raw token bytes. Rejects empty tokens.
    pub fn new(bytes: Vec<u8>) -> Result<Token, TokenError> {
        if bytes.is_empty() {
            return Err(TokenError::Empty);
        }
        Ok(Token(bytes))
    }

    /// Borrow the secret bytes. Named `expose` so call sites are greppable.
    pub fn expose(&self) -> &[u8] {
        &self.0
    }

    #[cfg(test)]
    pub(crate) fn zero_out(&mut self) {
        for b in self.0.iter_mut() {
            *b = 0;
        }
    }
}

impl Drop for Token {
    fn drop(&mut self) {
        for b in self.0.iter_mut() {
            *b = 0;
        }
    }
}

/// HMAC-SHA256 per RFC 2104, block size 64.
fn hmac_sha256_raw(key: &[u8], data: &[u8]) -> [u8; 32] {
    const BLOCK: usize = 64;
    let mut k = [0u8; BLOCK];
    if key.len() > BLOCK {
        let h = Sha256::digest(key);
        k[..32].copy_from_slice(&h);
    } else {
        k[..key.len()].copy_from_slice(key);
    }
    let mut ipad = [0x36u8; BLOCK];
    let mut opad = [0x5cu8; BLOCK];
    for i in 0..BLOCK {
        ipad[i] ^= k[i];
        opad[i] ^= k[i];
    }
    let mut inner = Sha256::new();
    inner.update(ipad);
    inner.update(data);
    let inner = inner.finalize();
    let mut outer = Sha256::new();
    outer.update(opad);
    outer.update(inner);
    outer.finalize().into()
}

fn mac(token: &Token, tag: &[u8], a: &NonceBytes, b: &NonceBytes) -> [u8; 32] {
    let mut msg = Vec::with_capacity(tag.len() + 2 * NONCE_LEN);
    msg.extend_from_slice(tag);
    msg.extend_from_slice(a);
    msg.extend_from_slice(b);
    hmac_sha256_raw(token.expose(), &msg)
}

/// Client MAC: `HMAC(token, "client" || nonce_s || nonce_c)`.
pub fn mac_client(token: &Token, nonce_s: &NonceBytes, nonce_c: &NonceBytes) -> [u8; 32] {
    mac(token, b"client", nonce_s, nonce_c)
}

/// Server MAC: `HMAC(token, "server" || nonce_c || nonce_s)`.
pub fn mac_server(token: &Token, nonce_c: &NonceBytes, nonce_s: &NonceBytes) -> [u8; 32] {
    mac(token, b"server", nonce_c, nonce_s)
}

/// Constant-time byte-slice equality. Length difference is folded into the
/// accumulator, so different lengths never compare equal.
pub fn ct_eq(a: &[u8], b: &[u8]) -> bool {
    let mut acc = 0u8;
    for (x, y) in a.iter().zip(b.iter()) {
        acc |= x ^ y;
    }
    acc |= (a.len() ^ b.len()) as u8;
    acc == 0
}

/// Server step 2: verify the client's MAC in constant time.
pub fn server_verify_client(
    token: &Token,
    nonce_s: &NonceBytes,
    nonce_c: &NonceBytes,
    client_mac: &[u8; 32],
) -> bool {
    ct_eq(&mac_client(token, nonce_s, nonce_c), client_mac)
}

/// Server step 3: the MAC the server sends back to the client.
pub fn server_finish_mac(token: &Token, nonce_c: &NonceBytes, nonce_s: &NonceBytes) -> [u8; 32] {
    mac_server(token, nonce_c, nonce_s)
}

/// Client step 4: verify the server's MAC in constant time.
pub fn client_verify_server(
    token: &Token,
    nonce_c: &NonceBytes,
    nonce_s: &NonceBytes,
    server_mac: &[u8; 32],
) -> bool {
    ct_eq(&mac_server(token, nonce_c, nonce_s), server_mac)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn token(bytes: &[u8]) -> Token {
        Token::new(bytes.to_vec()).expect("test token")
    }

    fn hex(s: &str) -> [u8; 32] {
        let mut out = [0u8; 32];
        for (i, b) in out.iter_mut().enumerate() {
            *b = u8::from_str_radix(&s[2 * i..2 * i + 2], 16).expect("hex");
        }
        out
    }

    /// Deterministic nonce source for tests: 0x01, 0x02, ... filling the array.
    struct DeterministicNonce(u8);

    impl NonceSource for DeterministicNonce {
        fn generate(&mut self) -> NonceBytes {
            let n = [self.0; NONCE_LEN];
            self.0 += 1;
            n
        }
    }

    #[test]
    fn hmac_rfc4231_vectors() {
        // Test Case 1
        let k1 = [0x0bu8; 20];
        assert_eq!(
            hmac_sha256_raw(&k1, b"Hi There"),
            hex("b0344c61d8db38535ca8afceaf0bf12b881dc200c9833da726e9376c2e32cff7")
        );
        // Test Case 2
        assert_eq!(
            hmac_sha256_raw(b"Jefe", b"what do ya want for nothing?"),
            hex("5bdcc146bf60754e6a042426089575c75a003f089d2739839dec58b964ec3843")
        );
        // Test Case 6: key larger than block size
        let k6 = [0xaau8; 131];
        assert_eq!(
            hmac_sha256_raw(
                &k6,
                b"Test Using Larger Than Block-Size Key - Hash Key First"
            ),
            hex("60e431591ee0b67f0d8a26aacbf5b77f8e0bc6213728c5140546040f0ee37f54")
        );
        // Test Case 7
        let d7 = b"This is a test using a larger than block-size key and a larger than block-size data. The key needs to be hashed before being used by the HMAC algorithm.";
        assert_eq!(
            hmac_sha256_raw(&k6, d7),
            hex("9b09ffa71b942fcb27635fbcd5b0e944bfdc63644f0713938a7f51535c3a35e2")
        );
    }

    #[test]
    fn client_and_server_macs_differ() {
        let t = token(b"secret");
        let ns = [1u8; NONCE_LEN];
        let nc = [2u8; NONCE_LEN];
        assert_ne!(mac_client(&t, &ns, &nc), mac_server(&t, &nc, &ns));
    }

    #[test]
    fn server_verify_client_accepts_and_rejects() {
        let t = token(b"secret");
        let ns = [1u8; NONCE_LEN];
        let nc = [2u8; NONCE_LEN];
        let good = mac_client(&t, &ns, &nc);
        assert!(server_verify_client(&t, &ns, &nc, &good));
        let mut bad = good;
        bad[0] ^= 1;
        assert!(!server_verify_client(&t, &ns, &nc, &bad));
    }

    #[test]
    fn ct_eq_cases() {
        assert!(ct_eq(b"", b""));
        assert!(ct_eq(b"abc", b"abc"));
        assert!(!ct_eq(b"abc", b"abd"));
        assert!(!ct_eq(b"abc", b"ab"));
    }

    #[test]
    fn happy_handshake_both_directions() {
        let t = token(b"secret");
        let mut server_nonce = DeterministicNonce(1);
        let mut client_nonce = DeterministicNonce(2);
        let ns = server_nonce.generate();
        let nc = client_nonce.generate();

        let cm = mac_client(&t, &ns, &nc);
        assert!(server_verify_client(&t, &ns, &nc, &cm));
        let sm = server_finish_mac(&t, &nc, &ns);
        assert!(client_verify_server(&t, &nc, &ns, &sm));
    }

    #[test]
    fn wrong_token_client_rejected() {
        let t = token(b"secret");
        let wrong = token(b"secret!");
        let ns = [1u8; NONCE_LEN];
        let nc = [2u8; NONCE_LEN];
        let cm = mac_client(&t, &ns, &nc);
        assert!(!server_verify_client(&wrong, &ns, &nc, &cm));
    }

    #[test]
    fn wrong_token_server_rejected_by_client() {
        let good = token(b"secret");
        let wrong = token(b"secret!");
        let ns = [1u8; NONCE_LEN];
        let nc = [2u8; NONCE_LEN];
        let sm = mac_server(&good, &nc, &ns);
        assert!(!client_verify_server(&wrong, &nc, &ns, &sm));
    }

    #[test]
    fn nonce_substitution_fails() {
        let t = token(b"secret");
        let ns1 = [1u8; NONCE_LEN];
        let ns2 = [9u8; NONCE_LEN];
        let nc = [2u8; NONCE_LEN];
        let cm = mac_client(&t, &ns1, &nc);
        assert!(!server_verify_client(&t, &ns2, &nc, &cm));
        // Order matters: swapped nonces must not verify.
        assert!(!server_verify_client(&t, &nc, &ns1, &cm));
    }

    #[test]
    fn replay_with_fresh_nonces_produces_different_macs() {
        let t = token(b"secret");
        let mut server_nonce = DeterministicNonce(1);
        let mut client_nonce = DeterministicNonce(2);

        let ns1 = server_nonce.generate();
        let nc1 = client_nonce.generate();
        let ns2 = server_nonce.generate();
        let nc2 = client_nonce.generate();

        assert_ne!(mac_client(&t, &ns1, &nc1), mac_client(&t, &ns2, &nc2));
        assert_ne!(mac_server(&t, &nc1, &ns1), mac_server(&t, &nc2, &ns2));
    }

    #[test]
    fn token_zero_out_for_test() {
        let mut t = token(b"secret");
        assert_eq!(t.expose(), b"secret");
        t.zero_out();
        assert!(t.expose().iter().all(|&b| b == 0));
    }

    #[test]
    fn empty_token_rejected() {
        assert!(matches!(Token::new(Vec::new()), Err(TokenError::Empty)));
    }
}
