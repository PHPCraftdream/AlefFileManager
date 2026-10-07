// SPDX-License-Identifier: MIT OR Apache-2.0
import { decodeBase64, encodeBase64 } from '../core/base64.ts';
import { call } from '../core/transport.ts';
import type { Cancelable } from '../desktop/app.ts';

/** The hashes of `digest`, `hmac`, `hkdf` and `pbkdf2` (`sha-1` is for what is old). */
export type HashName = 'sha-1' | 'sha-256' | 'sha-384' | 'sha-512';

/** The ciphers of `seal` and `open`. */
export type CipherName = 'aes-128-gcm' | 'aes-256-gcm' | 'chacha20-poly1305';

type Data = Uint8Array<ArrayBuffer> | string;
type Key = Uint8Array<ArrayBuffer>;

export interface HkdfOptions extends Cancelable {
  salt?: Uint8Array;
  info?: Uint8Array;
  /** How many bytes to derive (1 to 1024). */
  length: number;
}

export interface Argon2Options extends Cancelable {
  /** Memory in KiB (default 19456, up to 1 GiB). */
  memoryKiB?: number;
  /** Passes (default 2). */
  iterations?: number;
  /** Lanes (default 1). */
  parallelism?: number;
  /** The length of the hash in bytes (default 32). */
  length?: number;
}

export interface ScryptOptions extends Cancelable {
  /** The base 2 logarithm of the cost N (default 17, so N = 131072). */
  logN?: number;
  /** The size of a block (default 8). */
  r?: number;
  /** The parallelism (default 1). */
  p?: number;
  /** The length of the hash in bytes (default 32). */
  length?: number;
}

export interface SealOptions extends Cancelable {
  /** Associated data: authenticated, not hidden; `open` must be given the same. */
  aad?: Uint8Array;
}

export interface Ed25519Keys {
  /** The seed of 32 bytes that makes the key pair: keep it secret. */
  privateKey: Uint8Array<ArrayBuffer>;
  /** The public key of 32 bytes. */
  publicKey: Uint8Array<ArrayBuffer>;
}

const encoder = new TextEncoder();
const bytesOf = (data: Data): Uint8Array<ArrayBuffer> => (typeof data === 'string' ? encoder.encode(data) : data);
const text = (bytes: Uint8Array | undefined): string | undefined => (bytes === undefined ? undefined : encodeBase64(bytes));

/**
 * What an application needs and its page cannot do for itself: the WebCrypto of the engine is not in the
 * pages of an application (even `crypto.getRandomValues` is missing). Data is bytes (a string is its
 * UTF-8), the results are bytes. Nothing here touches the machine, so no right is asked for.
 */
export const crypto = {
  /** Random bytes from the system (up to 1 MiB at a time). */
  random: (length: number, options: Cancelable = {}): Promise<Uint8Array<ArrayBuffer>> =>
    call<Uint8Array<ArrayBuffer>>('crypto.random', { length }, options),

  digest: (algorithm: HashName, data: Data, options: Cancelable = {}): Promise<Uint8Array<ArrayBuffer>> =>
    call<Uint8Array<ArrayBuffer>>('crypto.digest', { algorithm }, { ...options, body: bytesOf(data) }),

  hmac: (algorithm: HashName, key: Key, data: Data, options: Cancelable = {}): Promise<Uint8Array<ArrayBuffer>> =>
    call<Uint8Array<ArrayBuffer>>('crypto.hmac', { algorithm, key: encodeBase64(key) }, { ...options, body: bytesOf(data) }),

  /** Whether `tag` is the HMAC of `data`, compared in constant time. */
  hmacVerify: async (algorithm: HashName, key: Key, data: Data, tag: Uint8Array, options: Cancelable = {}): Promise<boolean> => {
    const { valid } = await call<{ valid: boolean }>(
      'crypto.hmacVerify',
      { algorithm, key: encodeBase64(key), tag: encodeBase64(tag) },
      { ...options, body: bytesOf(data) },
    );
    return valid;
  },

  /** HKDF (RFC 5869): keys from a secret of high entropy. */
  hkdf: (algorithm: HashName, secret: Data, options: HkdfOptions): Promise<Uint8Array<ArrayBuffer>> => {
    const { salt, info, length, ...rest } = options;
    return call<Uint8Array<ArrayBuffer>>(
      'crypto.hkdf',
      { algorithm, salt: text(salt), info: text(info), length },
      { ...rest, body: bytesOf(secret) },
    );
  },

  /** PBKDF2 (up to 10 million iterations): for what must interoperate; a new password hash is `argon2id`. */
  pbkdf2: (
    algorithm: HashName,
    password: Data,
    salt: Uint8Array,
    iterations: number,
    length: number,
    options: Cancelable = {},
  ): Promise<Uint8Array<ArrayBuffer>> =>
    call<Uint8Array<ArrayBuffer>>(
      'crypto.pbkdf2',
      { algorithm, salt: encodeBase64(salt), iterations, length },
      { ...options, body: bytesOf(password) },
    ),

  /** Argon2id (RFC 9106) with the cost the advice of OWASP gives unless told otherwise; the salt has 8 bytes or more. */
  argon2id: (password: Data, salt: Uint8Array, options: Argon2Options = {}): Promise<Uint8Array<ArrayBuffer>> => {
    const { memoryKiB, iterations, parallelism, length, ...rest } = options;
    return call<Uint8Array<ArrayBuffer>>(
      'crypto.argon2id',
      { salt: encodeBase64(salt), memoryKiB, iterations, parallelism, length },
      { ...rest, body: bytesOf(password) },
    );
  },

  /** scrypt (RFC 7914) with the cost the advice of OWASP gives unless told otherwise. */
  scrypt: (password: Data, salt: Uint8Array, options: ScryptOptions = {}): Promise<Uint8Array<ArrayBuffer>> => {
    const { logN, r, p, length, ...rest } = options;
    return call<Uint8Array<ArrayBuffer>>(
      'crypto.scrypt',
      { salt: encodeBase64(salt), logN, r, p, length },
      { ...rest, body: bytesOf(password) },
    );
  },

  /**
   * Encrypts and authenticates `data`. The key has 16 bytes (`aes-128-gcm`) or 32 (the others). The
   * result is the nonce (12 bytes, new every time), the encrypted text and the tag (16 bytes), in this order.
   */
  seal: (cipher: CipherName, key: Key, data: Data, options: SealOptions = {}): Promise<Uint8Array<ArrayBuffer>> => {
    const { aad, ...rest } = options;
    return call<Uint8Array<ArrayBuffer>>(
      'crypto.seal',
      { algorithm: cipher, key: encodeBase64(key), aad: text(aad) },
      { ...rest, body: bytesOf(data) },
    );
  },

  /** What `seal` made, opened; a message that was changed, or another key or `aad`, is `INVALID_ARGUMENT`. */
  open: (cipher: CipherName, key: Key, sealed: Uint8Array<ArrayBuffer>, options: SealOptions = {}): Promise<Uint8Array<ArrayBuffer>> => {
    const { aad, ...rest } = options;
    return call<Uint8Array<ArrayBuffer>>(
      'crypto.open',
      { algorithm: cipher, key: encodeBase64(key), aad: text(aad) },
      { ...rest, body: sealed },
    );
  },

  ed25519Generate: async (options: Cancelable = {}): Promise<Ed25519Keys> => {
    const made = await call<{ privateKey: string; publicKey: string }>('crypto.ed25519Generate', null, options);
    return { privateKey: decodeBase64(made.privateKey), publicKey: decodeBase64(made.publicKey) };
  },

  /** The signature (64 bytes) of `message` by the key a seed of 32 bytes makes. */
  ed25519Sign: (privateKey: Key, message: Data, options: Cancelable = {}): Promise<Uint8Array<ArrayBuffer>> =>
    call<Uint8Array<ArrayBuffer>>('crypto.ed25519Sign', { privateKey: encodeBase64(privateKey) }, { ...options, body: bytesOf(message) }),

  /** Whether `signature` is that of `message` by the owner of `publicKey`. */
  ed25519Verify: async (publicKey: Uint8Array, message: Data, signature: Uint8Array, options: Cancelable = {}): Promise<boolean> => {
    const { valid } = await call<{ valid: boolean }>(
      'crypto.ed25519Verify',
      { publicKey: encodeBase64(publicKey), signature: encodeBase64(signature) },
      { ...options, body: bytesOf(message) },
    );
    return valid;
  },
};
