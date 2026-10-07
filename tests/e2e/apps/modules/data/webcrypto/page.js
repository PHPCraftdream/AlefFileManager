// The WebCrypto of the engine, tried in the origin of an application: what is there (and what is not) is
// reported per algorithm in the detail of each check ("supported ..." or "unsupported: ..."), and the run
// never fails for it. Today there is none: the feature of Servo is off and the origin is not a secure
// context (`crypto` is not even defined); the module `crypto` of the runtime gives what an application
// needs. When this page starts to say "supported", the decision of the stage document is to be revisited.
import { api, guard, suite, verdict } from './harness.js';

const hex = bytes => [...new Uint8Array(bytes)].map(byte => byte.toString(16).padStart(2, '0')).join('');
const unhex = text => new Uint8Array(text.match(/../g).map(pair => parseInt(pair, 16)));
const encode = text => new TextEncoder().encode(text);
const same = (left, right) => hex(left) === hex(right);

async function probe(check, name, body) {
  await check(`webcrypto-${name}`, async () => {
    try {
      return `supported ${(await body()) ?? ''}`.trim();
    } catch (error) {
      return `unsupported: ${error?.name ?? 'Error'} ${error?.message ?? ''}`.trim();
    }
  });
}

const expect = (what, actual, wanted) => {
  if (actual !== wanted) throw new Error(`${what}: ${actual}, expected ${wanted}`);
};

/** Sign, verify and fail to verify a tampered message. */
async function signature(algorithm, usages = ['sign', 'verify'], signing = algorithm) {
  const pair = await crypto.subtle.generateKey(algorithm, true, usages);
  const message = encode('a message to sign');
  const proof = await crypto.subtle.sign(signing, pair.privateKey, message);
  if (!await crypto.subtle.verify(signing, pair.publicKey, proof, message)) throw new Error('a good signature did not verify');
  if (await crypto.subtle.verify(signing, pair.publicKey, proof, encode('another message'))) throw new Error('a tampered message verified');
  return `${proof.byteLength}-byte signature`;
}

async function agreement(algorithm, length) {
  const [one, two] = await Promise.all([1, 2].map(() => crypto.subtle.generateKey(algorithm, true, ['deriveBits'])));
  const ab = await crypto.subtle.deriveBits({ ...algorithm, public: two.publicKey }, one.privateKey, length);
  const ba = await crypto.subtle.deriveBits({ ...algorithm, public: one.publicKey }, two.privateKey, length);
  if (!same(ab, ba)) throw new Error('the two sides derived different bits');
  return `${length} bits`;
}

async function cipher(algorithm, parameters) {
  const key = await crypto.subtle.generateKey(algorithm, true, ['encrypt', 'decrypt']);
  const plain = encode('héllo — мир 🌍, a message that is longer than one block of sixteen bytes');
  const sealed = await crypto.subtle.encrypt(parameters, key, plain);
  const opened = await crypto.subtle.decrypt(parameters, key, sealed);
  if (!same(opened, plain)) throw new Error('what was decrypted is not what was encrypted');
  return `${sealed.byteLength} bytes for ${plain.length}`;
}

async function main() {
  const { check, failed } = suite();
  const subtle = globalThis.crypto?.subtle;

  await check('webcrypto-state-of-the-engine', async () => {
    const present = typeof globalThis.crypto !== 'undefined';
    return `${present ? 'present' : 'absent'}: crypto ${typeof globalThis.crypto}, subtle ${typeof globalThis.crypto?.subtle}, isSecureContext ${globalThis.isSecureContext}, origin ${location.origin}`;
  });

  await probe(check, 'random-values-and-uuid', async () => {
    const one = crypto.getRandomValues(new Uint8Array(32));
    const two = crypto.getRandomValues(new Uint8Array(32));
    if (one.every(byte => byte === 0) || same(one, two)) throw new Error('not random');
    if (!/^[0-9a-f]{8}-[0-9a-f]{4}-4[0-9a-f]{3}-[89ab][0-9a-f]{3}-[0-9a-f]{12}$/.test(crypto.randomUUID())) throw new Error('randomUUID');
    return 'getRandomValues, randomUUID';
  });

  const digests = {
    'SHA-1': 'a9993e364706816aba3e25717850c26c9cd0d89d',
    'SHA-256': 'ba7816bf8f01cfea414140de5dae2223b00361a396177a9cb410ff61f20015ad',
    'SHA-384': 'cb00753f45a35e8bb5a03d699ac65007272c32ab0eded1631a8b605a43ff5bed8086072ba1e7cc2358baeca134c825a7',
    'SHA-512': 'ddaf35a193617abacc417349ae20413112e6fa4e89a97ea20a9eeee64b55d39a2192992a274fc1a836ba3c23a3feebbd454d4423643ce80e2a9ac94fa54ca49f',
  };
  for (const [name, wanted] of Object.entries(digests)) {
    await probe(check, `digest-${name.toLowerCase()}`, async () => {
      expect(name, hex(await subtle.digest(name, encode('abc'))), wanted);
    });
  }

  const hmac = async hash => {
    const key = await subtle.importKey('raw', encode('key'), { name: 'HMAC', hash }, false, ['sign', 'verify']);
    const proof = await subtle.sign('HMAC', key, encode('The quick brown fox jumps over the lazy dog'));
    if (!await subtle.verify('HMAC', key, proof, encode('The quick brown fox jumps over the lazy dog'))) throw new Error('verify');
    return hex(proof);
  };
  await probe(check, 'hmac-sha-256', async () => {
    expect('HMAC-SHA-256', await hmac('SHA-256'), 'f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8');
  });
  await probe(check, 'hmac-sha-512', async () => {
    await hmac('SHA-512');
  });

  await probe(check, 'pbkdf2-sha-256', async () => {
    const base = await subtle.importKey('raw', encode('password'), 'PBKDF2', false, ['deriveBits']);
    const bits = await subtle.deriveBits({ name: 'PBKDF2', hash: 'SHA-256', salt: encode('salt'), iterations: 1 }, base, 256);
    expect('PBKDF2', hex(bits), '120fb6cffcf8b32c43e7225256c4f837a86548c92ccc35480805987cb70be17b');
  });

  await probe(check, 'hkdf-sha-256', async () => {
    const base = await subtle.importKey('raw', new Uint8Array(22).fill(0x0b), 'HKDF', false, ['deriveBits']);
    const bits = await subtle.deriveBits({ name: 'HKDF', hash: 'SHA-256', salt: unhex('000102030405060708090a0b0c'), info: unhex('f0f1f2f3f4f5f6f7f8f9') }, base, 42 * 8);
    expect('HKDF', hex(bits), '3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf34007208d5b887185865');
  });

  await probe(check, 'aes-gcm-256', async () => {
    const key = await subtle.generateKey({ name: 'AES-GCM', length: 256 }, true, ['encrypt', 'decrypt']);
    const iv = crypto.getRandomValues(new Uint8Array(12));
    const plain = encode('héllo — мир 🌍');
    const parameters = { name: 'AES-GCM', iv, additionalData: encode('header') };
    const sealed = new Uint8Array(await subtle.encrypt(parameters, key, plain));
    if (sealed.length !== plain.length + 16) throw new Error(`${sealed.length} bytes, expected ${plain.length + 16}`);
    if (!same(await subtle.decrypt(parameters, key, sealed), plain)) throw new Error('round trip');
    const tampered = sealed.slice();
    tampered[0] ^= 1;
    let refused = false;
    try {
      await subtle.decrypt(parameters, key, tampered);
    } catch {
      refused = true;
    }
    if (!refused) throw new Error('a tampered message was decrypted');
    const raw = await subtle.exportKey('raw', key);
    if (raw.byteLength !== 32) throw new Error('exportKey raw');
    return 'tag 128 bit, tampering refused, raw export';
  });
  await probe(check, 'aes-cbc-256', () => cipher({ name: 'AES-CBC', length: 256 }, { name: 'AES-CBC', iv: crypto.getRandomValues(new Uint8Array(16)) }));
  await probe(check, 'aes-ctr-256', () => cipher({ name: 'AES-CTR', length: 256 }, { name: 'AES-CTR', counter: crypto.getRandomValues(new Uint8Array(16)), length: 64 }));
  await probe(check, 'aes-kw-256', async () => {
    const wrapping = await subtle.generateKey({ name: 'AES-KW', length: 256 }, true, ['wrapKey', 'unwrapKey']);
    const inner = await subtle.generateKey({ name: 'AES-GCM', length: 256 }, true, ['encrypt', 'decrypt']);
    const wrapped = await subtle.wrapKey('raw', inner, wrapping, 'AES-KW');
    const back = await subtle.unwrapKey('raw', wrapped, wrapping, 'AES-KW', 'AES-GCM', true, ['encrypt']);
    if (!same(await subtle.exportKey('raw', back), await subtle.exportKey('raw', inner))) throw new Error('unwrapped another key');
  });

  await probe(check, 'derive-key-pbkdf2-to-aes-gcm', async () => {
    const base = await subtle.importKey('raw', encode('a passphrase'), 'PBKDF2', false, ['deriveKey']);
    const key = await subtle.deriveKey({ name: 'PBKDF2', hash: 'SHA-256', salt: crypto.getRandomValues(new Uint8Array(16)), iterations: 1000 }, base, { name: 'AES-GCM', length: 256 }, false, ['encrypt', 'decrypt']);
    const iv = crypto.getRandomValues(new Uint8Array(12));
    const sealed = await subtle.encrypt({ name: 'AES-GCM', iv }, key, encode('secret'));
    if (new TextDecoder().decode(await subtle.decrypt({ name: 'AES-GCM', iv }, key, sealed)) !== 'secret') throw new Error('round trip');
  });

  for (const curve of ['P-256', 'P-384', 'P-521']) {
    await probe(check, `ecdsa-${curve.toLowerCase()}`, () => signature({ name: 'ECDSA', namedCurve: curve }, ['sign', 'verify'], { name: 'ECDSA', hash: curve === 'P-256' ? 'SHA-256' : 'SHA-384' }));
  }
  for (const curve of ['P-256', 'P-384']) {
    await probe(check, `ecdh-${curve.toLowerCase()}`, () => agreement({ name: 'ECDH', namedCurve: curve }, curve === 'P-256' ? 256 : 384));
  }
  await probe(check, 'ecdsa-p-256-export-import-jwk-and-spki', async () => {
    const pair = await subtle.generateKey({ name: 'ECDSA', namedCurve: 'P-256' }, true, ['sign', 'verify']);
    const jwk = await subtle.exportKey('jwk', pair.publicKey);
    const spki = await subtle.exportKey('spki', pair.publicKey);
    await subtle.importKey('jwk', jwk, { name: 'ECDSA', namedCurve: 'P-256' }, true, ['verify']);
    await subtle.importKey('spki', spki, { name: 'ECDSA', namedCurve: 'P-256' }, true, ['verify']);
    return `jwk kty ${jwk.kty}, spki ${spki.byteLength} bytes`;
  });

  await probe(check, 'ed25519', () => signature({ name: 'Ed25519' }, ['sign', 'verify']));
  await probe(check, 'x25519', () => agreement({ name: 'X25519' }, 256));

  const started = performance.now();
  const rsaTime = () => `${Math.round(performance.now() - started)} ms since the first RSA key`;
  await probe(check, 'rsassa-pkcs1-v1_5-2048', async () => `${await signature({ name: 'RSASSA-PKCS1-v1_5', modulusLength: 2048, publicExponent: new Uint8Array([1, 0, 1]), hash: 'SHA-256' }, ['sign', 'verify'], { name: 'RSASSA-PKCS1-v1_5' })}, ${rsaTime()}`);
  await probe(check, 'rsa-pss-2048', async () => `${await signature({ name: 'RSA-PSS', modulusLength: 2048, publicExponent: new Uint8Array([1, 0, 1]), hash: 'SHA-256' }, ['sign', 'verify'], { name: 'RSA-PSS', saltLength: 32 })}, ${rsaTime()}`);
  await probe(check, 'rsa-oaep-2048', async () => {
    const pair = await subtle.generateKey({ name: 'RSA-OAEP', modulusLength: 2048, publicExponent: new Uint8Array([1, 0, 1]), hash: 'SHA-256' }, true, ['encrypt', 'decrypt']);
    const sealed = await subtle.encrypt({ name: 'RSA-OAEP' }, pair.publicKey, encode('short secret'));
    if (new TextDecoder().decode(await subtle.decrypt({ name: 'RSA-OAEP' }, pair.privateKey, sealed)) !== 'short secret') throw new Error('round trip');
    return rsaTime();
  });

  await verdict(failed());
  await api.app.quit(0);
}

guard(main);
