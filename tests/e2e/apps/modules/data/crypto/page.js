// The module `crypto` through the real transport. The runner works out what is expected with the OpenSSL
// of Node (an oracle that is not ours) and the page compares: digests, MACs, key derivation, a big body,
// a message sealed by Node that the page opens, one sealed by the page that the runner opens, signatures.
import { api, guard, pattern, rejection, report, suite, verdict } from './harness.js';

const { crypto } = api;

const hex = bytes => [...new Uint8Array(bytes)].map(byte => byte.toString(16).padStart(2, '0')).join('');
const unhex = text => new Uint8Array((text.match(/../g) ?? []).map(pair => parseInt(pair, 16)));
const same = (what, actual, expected) => {
  if (hex(actual) !== expected) throw new Error(`${what}: ${hex(actual).slice(0, 80)}, expected ${expected.slice(0, 80)}`);
};

const expectCode = async (what, promise, code) => {
  const error = await rejection(promise);
  if (error?.code !== code) throw new Error(`${what}: ${error?.code ?? 'it succeeded'}, expected ${code}`);
};

async function main() {
  const t = await (await fetch('targets.json')).json();
  const { check, failed } = suite();
  const salt = unhex(t.saltHex);
  const text = 'héllo — мир 🌍';

  await check('crypto-digests-and-macs-match-the-openssl-of-node', async () => {
    for (const name of ['sha-1', 'sha-256', 'sha-384', 'sha-512']) {
      same(`digest ${name}`, await crypto.digest(name, text), t.digest[name]);
      same(`hmac ${name}`, await crypto.hmac(name, unhex(t.keyHex), text), t.hmac[name]);
    }
    if (!await crypto.hmacVerify('sha-256', unhex(t.keyHex), text, unhex(t.hmac['sha-256']))) throw new Error('a good tag did not verify');
    if (await crypto.hmacVerify('sha-256', unhex(t.keyHex), `${text}!`, unhex(t.hmac['sha-256']))) throw new Error('another message verified');
  });

  await check('crypto-a-big-body-is-digested-whole', async () => {
    const started = performance.now();
    same('digest of 8 MiB', await crypto.digest('sha-256', pattern(8 * 1024 * 1024)), t.bigDigest);
    return `${Math.round(performance.now() - started)} ms`;
  });

  await check('crypto-keys-derived-match-the-openssl-of-node', async () => {
    same('hkdf', await crypto.hkdf('sha-256', text, { salt, info: unhex(t.infoHex), length: 42 }), t.hkdf);
    same('pbkdf2', await crypto.pbkdf2('sha-256', text, salt, 1000, 48), t.pbkdf2);
    same('scrypt', await crypto.scrypt(text, salt, { logN: 10, r: 8, p: 1, length: 40 }), t.scrypt);
    if (t.argon2id) {
      same('argon2id', await crypto.argon2id(text, salt, { memoryKiB: 4096, iterations: 3, parallelism: 2, length: 40 }), t.argon2id);
    }
    return t.argon2id ? 'with argon2id' : 'argon2id: no oracle in this Node';
  });

  await check('crypto-random-bytes-are-random', async () => {
    const one = await crypto.random(64);
    const two = await crypto.random(64);
    if (one.length !== 64 || hex(one) === hex(two) || one.every(byte => byte === 0)) throw new Error('not random');
    if ((await crypto.random(0)).length !== 0) throw new Error('no bytes asked, some given');
    await expectCode('too many', crypto.random(1024 * 1024 + 1), 'INVALID_ARGUMENT');
  });

  await check('crypto-a-message-sealed-by-node-is-opened-and-the-page-seals-for-node', async () => {
    for (const cipher of Object.keys(t.sealed)) {
      const key = unhex(t.sealed[cipher].keyHex);
      const aad = unhex(t.sealed[cipher].aadHex);
      const opened = await crypto.open(cipher, key, unhex(t.sealed[cipher].messageHex), { aad });
      if (new TextDecoder().decode(opened) !== text) throw new Error(`${cipher}: opened another text`);
      await expectCode(`${cipher} without the aad`, crypto.open(cipher, key, unhex(t.sealed[cipher].messageHex)), 'INVALID_ARGUMENT');
      const mine = await crypto.seal(cipher, key, text, { aad });
      await report(`sealed ${cipher} ${hex(mine)}`);
      if (hex(await crypto.open(cipher, key, mine, { aad })) !== hex(new TextEncoder().encode(text))) throw new Error(`${cipher}: no round trip`);
    }
  });

  await check('crypto-ed25519-signs-as-node-does-and-checks-what-node-signed', async () => {
    const seed = unhex(t.ed25519.seedHex);
    const message = new TextEncoder().encode(text);
    same('signature', await crypto.ed25519Sign(seed, message), t.ed25519.signature);
    if (!await crypto.ed25519Verify(unhex(t.ed25519.publicHex), message, unhex(t.ed25519.signature))) throw new Error('the signature of Node did not verify');
    if (await crypto.ed25519Verify(unhex(t.ed25519.publicHex), `${text}!`, unhex(t.ed25519.signature))) throw new Error('another message verified');
    const made = await crypto.ed25519Generate();
    const proof = await crypto.ed25519Sign(made.privateKey, message);
    await report(`ed25519 ${hex(made.publicKey)} ${hex(proof)}`);
    if (!await crypto.ed25519Verify(made.publicKey, message, proof)) throw new Error('a generated pair does not work');
  });

  await check('crypto-what-is-asked-wrongly-is-told', async () => {
    await expectCode('a hash', crypto.digest('md5', 'x'), 'INVALID_ARGUMENT');
    await expectCode('a short salt', crypto.argon2id('x', new Uint8Array(4)), 'INVALID_ARGUMENT');
    await expectCode('a key of the wrong length', crypto.seal('aes-256-gcm', new Uint8Array(16), 'x'), 'INVALID_ARGUMENT');
    await expectCode('too much memory', crypto.argon2id('x', salt, { memoryKiB: 2 * 1024 * 1024 }), 'INVALID_ARGUMENT');
  });

  await verdict(failed());
  await api.app.quit(0);
}

guard(main);
