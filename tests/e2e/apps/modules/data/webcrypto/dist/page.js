// The WebCrypto of the engine, tried in the origin of an application, in three modes of the right:
// `allowed` — the user granted the right and every algorithm of the Servo feature must work;
// `substituted` — the user chose a stand-in; `undeclared` — the manifest never asked (deny also
// substitutes). In the last two every `crypto.subtle` method rejects with `NotSupportedError` and a
// message about Alef, the stand-in is sealed against redefinition, and `crypto.getRandomValues`
// and `crypto.randomUUID` stay real in all three modes.
import { api, guard, suite, verdict } from './harness.js';

const hex = bytes => [...new Uint8Array(bytes)].map(byte => byte.toString(16).padStart(2, '0')).join('');
const unhex = text => new Uint8Array(text.match(/../g).map(pair => parseInt(pair, 16)));
const encode = text => new TextEncoder().encode(text);
const same = (left, right) => hex(left) === hex(right);

const expect = (what, actual, wanted) => {
  if (actual !== wanted) throw new Error(`${what}: ${actual}, expected ${wanted}`);
};

// The only place where the mode matters: in `allowed` the body runs and its detail is reported;
// otherwise the body must reject with the stand-in error of Alef and the detail says so.
async function probe(check, mode, name, body) {
  await check(`webcrypto-${name}`, async () => {
    if (mode === 'allowed') return `supported ${(await body()) ?? ''}`.trim();
    const error = await body().then(
      () => { throw new Error('the substituted crypto.subtle let the call through'); },
      reason => reason,
    );
    expect(`${name}: error name`, error?.name, 'NotSupportedError');
    if (!String(error?.message ?? '').includes('substituted by Alef')) {
      throw new Error(`${name}: message "${error?.message}" does not mention the stand-in`);
    }
    return `substituted: ${error.name}`;
  });
}

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
  const t = await (await fetch('targets.json')).json();
  const mode = t.mode;
  const { check, failed } = suite();

  await check('webcrypto-state-of-the-engine', async () => {
    expect('crypto is defined', typeof crypto !== 'undefined', true);
    expect('crypto.subtle is an object', typeof crypto.subtle === 'object', true);
    return `${mode}: crypto ${typeof crypto}, subtle ${typeof crypto.subtle}, isSecureContext ${globalThis.isSecureContext}, origin ${location.origin}`;
  });

  // getRandomValues and randomUUID are not part of the right: they work in every mode.
  await check('webcrypto-random-values-and-uuid', async () => {
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
    await probe(check, mode, `digest-${name.toLowerCase()}`, async () => {
      expect(name, hex(await crypto.subtle.digest(name, encode('abc'))), wanted);
    });
  }

  const hmac = async hash => {
    const key = await crypto.subtle.importKey('raw', encode('key'), { name: 'HMAC', hash }, false, ['sign', 'verify']);
    const proof = await crypto.subtle.sign('HMAC', key, encode('The quick brown fox jumps over the lazy dog'));
    if (!await crypto.subtle.verify('HMAC', key, proof, encode('The quick brown fox jumps over the lazy dog'))) throw new Error('verify');
    return hex(proof);
  };
  await probe(check, mode, 'hmac-sha-256', async () => {
    expect('HMAC-SHA-256', await hmac('SHA-256'), 'f7bc83f430538424b13298e6aa6fb143ef4d59a14946175997479dbc2d1a3cd8');
  });
  await probe(check, mode, 'hmac-sha-512', async () => {
    await hmac('SHA-512');
  });

  await probe(check, mode, 'pbkdf2-sha-256', async () => {
    const base = await crypto.subtle.importKey('raw', encode('password'), 'PBKDF2', false, ['deriveBits']);
    const bits = await crypto.subtle.deriveBits({ name: 'PBKDF2', hash: 'SHA-256', salt: encode('salt'), iterations: 1 }, base, 256);
    expect('PBKDF2', hex(bits), '120fb6cffcf8b32c43e7225256c4f837a86548c92ccc35480805987cb70be17b');
  });

  await probe(check, mode, 'hkdf-sha-256', async () => {
    const base = await crypto.subtle.importKey('raw', new Uint8Array(22).fill(0x0b), 'HKDF', false, ['deriveBits']);
    const bits = await crypto.subtle.deriveBits({ name: 'HKDF', hash: 'SHA-256', salt: unhex('000102030405060708090a0b0c'), info: unhex('f0f1f2f3f4f5f6f7f8f9') }, base, 42 * 8);
    expect('HKDF', hex(bits), '3cb25f25faacd57a90434f64d0362f2a2d2d0a90cf1a5a4c5db02d56ecc4c5bf34007208d5b887185865');
  });

  await probe(check, mode, 'aes-gcm-256', async () => {
    const key = await crypto.subtle.generateKey({ name: 'AES-GCM', length: 256 }, true, ['encrypt', 'decrypt']);
    const iv = crypto.getRandomValues(new Uint8Array(12));
    const plain = encode('héllo — мир 🌍');
    const parameters = { name: 'AES-GCM', iv, additionalData: encode('header') };
    const sealed = new Uint8Array(await crypto.subtle.encrypt(parameters, key, plain));
    if (sealed.length !== plain.length + 16) throw new Error(`${sealed.length} bytes, expected ${plain.length + 16}`);
    if (!same(await crypto.subtle.decrypt(parameters, key, sealed), plain)) throw new Error('round trip');
    const tampered = sealed.slice();
    tampered[0] ^= 1;
    let refused = false;
    try {
      await crypto.subtle.decrypt(parameters, key, tampered);
    } catch {
      refused = true;
    }
    if (!refused) throw new Error('a tampered message was decrypted');
    const raw = await crypto.subtle.exportKey('raw', key);
    if (raw.byteLength !== 32) throw new Error('exportKey raw');
    return 'tag 128 bit, tampering refused, raw export';
  });
  await probe(check, mode, 'aes-cbc-256', () => cipher({ name: 'AES-CBC', length: 256 }, { name: 'AES-CBC', iv: crypto.getRandomValues(new Uint8Array(16)) }));
  await probe(check, mode, 'aes-ctr-256', () => cipher({ name: 'AES-CTR', length: 256 }, { name: 'AES-CTR', counter: crypto.getRandomValues(new Uint8Array(16)), length: 64 }));
  await probe(check, mode, 'aes-kw-256', async () => {
    const wrapping = await crypto.subtle.generateKey({ name: 'AES-KW', length: 256 }, true, ['wrapKey', 'unwrapKey']);
    const inner = await crypto.subtle.generateKey({ name: 'AES-GCM', length: 256 }, true, ['encrypt', 'decrypt']);
    const wrapped = await crypto.subtle.wrapKey('raw', inner, wrapping, 'AES-KW');
    const back = await crypto.subtle.unwrapKey('raw', wrapped, wrapping, 'AES-KW', 'AES-GCM', true, ['encrypt']);
    if (!same(await crypto.subtle.exportKey('raw', back), await crypto.subtle.exportKey('raw', inner))) throw new Error('unwrapped another key');
  });

  await probe(check, mode, 'derive-key-pbkdf2-to-aes-gcm', async () => {
    const base = await crypto.subtle.importKey('raw', encode('a passphrase'), 'PBKDF2', false, ['deriveKey']);
    const key = await crypto.subtle.deriveKey({ name: 'PBKDF2', hash: 'SHA-256', salt: crypto.getRandomValues(new Uint8Array(16)), iterations: 1000 }, base, { name: 'AES-GCM', length: 256 }, false, ['encrypt', 'decrypt']);
    const iv = crypto.getRandomValues(new Uint8Array(12));
    const sealed = await crypto.subtle.encrypt({ name: 'AES-GCM', iv }, key, encode('secret'));
    if (new TextDecoder().decode(await crypto.subtle.decrypt({ name: 'AES-GCM', iv }, key, sealed)) !== 'secret') throw new Error('round trip');
  });

  for (const curve of ['P-256', 'P-384', 'P-521']) {
    await probe(check, mode, `ecdsa-${curve.toLowerCase()}`, () => signature({ name: 'ECDSA', namedCurve: curve }, ['sign', 'verify'], { name: 'ECDSA', hash: curve === 'P-256' ? 'SHA-256' : 'SHA-384' }));
  }
  for (const curve of ['P-256', 'P-384']) {
    await probe(check, mode, `ecdh-${curve.toLowerCase()}`, () => agreement({ name: 'ECDH', namedCurve: curve }, curve === 'P-256' ? 256 : 384));
  }
  await probe(check, mode, 'ecdsa-p-256-export-import-jwk-and-spki', async () => {
    const pair = await crypto.subtle.generateKey({ name: 'ECDSA', namedCurve: 'P-256' }, true, ['sign', 'verify']);
    const jwk = await crypto.subtle.exportKey('jwk', pair.publicKey);
    const spki = await crypto.subtle.exportKey('spki', pair.publicKey);
    await crypto.subtle.importKey('jwk', jwk, { name: 'ECDSA', namedCurve: 'P-256' }, true, ['verify']);
    await crypto.subtle.importKey('spki', spki, { name: 'ECDSA', namedCurve: 'P-256' }, true, ['verify']);
    return `jwk kty ${jwk.kty}, spki ${spki.byteLength} bytes`;
  });

  await probe(check, mode, 'ed25519', () => signature({ name: 'Ed25519' }, ['sign', 'verify']));
  await probe(check, mode, 'x25519', () => agreement({ name: 'X25519' }, 256));

  const started = performance.now();
  const rsaTime = () => `${Math.round(performance.now() - started)} ms since the first RSA key`;
  await probe(check, mode, 'rsassa-pkcs1-v1_5-2048', async () => `${await signature({ name: 'RSASSA-PKCS1-v1_5', modulusLength: 2048, publicExponent: new Uint8Array([1, 0, 1]), hash: 'SHA-256' }, ['sign', 'verify'], { name: 'RSASSA-PKCS1-v1_5' })}, ${rsaTime()}`);
  await probe(check, mode, 'rsa-pss-2048', async () => `${await signature({ name: 'RSA-PSS', modulusLength: 2048, publicExponent: new Uint8Array([1, 0, 1]), hash: 'SHA-256' }, ['sign', 'verify'], { name: 'RSA-PSS', saltLength: 32 })}, ${rsaTime()}`);
  await probe(check, mode, 'rsa-oaep-2048', async () => {
    const pair = await crypto.subtle.generateKey({ name: 'RSA-OAEP', modulusLength: 2048, publicExponent: new Uint8Array([1, 0, 1]), hash: 'SHA-256' }, true, ['encrypt', 'decrypt']);
    const sealed = await crypto.subtle.encrypt({ name: 'RSA-OAEP' }, pair.publicKey, encode('short secret'));
    if (new TextDecoder().decode(await crypto.subtle.decrypt({ name: 'RSA-OAEP' }, pair.privateKey, sealed)) !== 'short secret') throw new Error('round trip');
    return rsaTime();
  });

  // The substitution is sealed on SubtleCrypto.prototype itself: a page can neither delete nor
  // redefine a stub, and the native object reached through the prototype getter rejects too.
  if (mode !== 'allowed') {
    await check('webcrypto-the-stand-in-is-sealed', async () => {
      const descriptor = Object.getOwnPropertyDescriptor(SubtleCrypto.prototype, 'digest');
      expect('digest is not configurable', descriptor?.configurable, false);
      expect('digest is not writable', descriptor?.writable, false);
      expect('delete SubtleCrypto.prototype.digest refuses', Reflect.deleteProperty(SubtleCrypto.prototype, 'digest'), false);
      expect('redefining SubtleCrypto.prototype.digest refuses', Reflect.defineProperty(SubtleCrypto.prototype, 'digest', { value: () => 1 }), false);
      const native = Object.getOwnPropertyDescriptor(Crypto.prototype, 'subtle').get.call(crypto);
      const bypassed = await native.digest('SHA-256', encode('abc')).then(
        () => { throw new Error('digest through the prototype getter reached the engine'); },
        reason => reason,
      );
      expect('digest through the prototype getter: error name', bypassed?.name, 'NotSupportedError');
      const error = await crypto.subtle.digest('SHA-256', encode('abc')).then(
        () => { throw new Error('digest reached the engine'); },
        reason => reason,
      );
      expect('digest: error name', error?.name, 'NotSupportedError');
      return 'the stubs are sealed on the prototype and both routes reject';
    });
  }

  // Every method of SubtleCrypto.prototype returns a promise rejected with the Alef DOMException;
  // the list is checked against the methods every WebCrypto engine has, so a missed stub fails.
  if (mode !== 'allowed') {
    await check('webcrypto-every-method-of-the-native-object-rejects', async () => {
      const names = Object.getOwnPropertyNames(SubtleCrypto.prototype)
        .filter(name => name !== 'constructor' && typeof Object.getOwnPropertyDescriptor(SubtleCrypto.prototype, name)?.value === 'function');
      const required = ['encrypt', 'decrypt', 'sign', 'verify', 'digest', 'generateKey', 'deriveKey', 'deriveBits', 'importKey', 'exportKey', 'wrapKey', 'unwrapKey'];
      const missing = required.filter(name => !names.includes(name));
      if (missing.length > 0) throw new Error('SubtleCrypto.prototype lacks: ' + missing.join(','));
      const offenders = [];
      for (const name of names) {
        let outcome;
        try {
          outcome = SubtleCrypto.prototype[name].call(crypto.subtle, new Uint8Array(1), {}, {}, 'x');
        } catch (error) {
          offenders.push(name + ' threw synchronously: ' + error?.name);
          continue;
        }
        if (!(outcome instanceof Promise)) { offenders.push(name + ' returned no promise'); continue; }
        const reason = await outcome.then(() => null, error => error ?? new Error('rejected without a reason'));
        if (reason === null) { offenders.push(name + ' resolved'); continue; }
        if (!(reason instanceof DOMException) || reason.name !== 'NotSupportedError' || !String(reason.message).includes('substituted by Alef')) {
          offenders.push(name + ': ' + reason?.constructor?.name + ' ' + reason?.name + ' "' + reason?.message + '"');
        }
      }
      if (offenders.length > 0) throw new Error('these methods did not reject with the Alef DOMException: ' + offenders.join('; '));
      return 'every method of ' + names.length + ' rejects with the Alef DOMException';
    });
  }

  await verdict(failed());
  await api.app.quit(0);
}

guard(main);
