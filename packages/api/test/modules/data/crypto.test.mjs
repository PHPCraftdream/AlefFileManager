// SPDX-License-Identifier: MIT OR Apache-2.0
import assert from 'node:assert/strict';
import test from 'node:test';
import { AlefError, crypto } from '../../../src/index.ts';
import { installRuntime } from '../../fake-runtime.mjs';

const replies = new Map();
const runtime = installRuntime({
  handler(request) {
    return replies.get(request.url.replace('native://call/', '')) ?? { json: null };
  },
});

async function sent(command, fn, reply) {
  replies.set(command, reply);
  const before = runtime.calls(command).length;
  const result = await fn();
  const calls = runtime.calls(command);
  assert.equal(calls.length, before + 1, `${command} was called once`);
  const request = calls.at(-1);
  return { result, args: runtime.argsOf(request), body: request.body };
}

const bytes = (...values) => new Uint8Array(values);
const b64 = text => Buffer.from(text, 'latin1').toString('base64');

test('random asks for a length and gives the bytes', async () => {
  const done = await sent('crypto.random', () => crypto.random(4), { bytes: bytes(1, 2, 3, 4) });
  assert.deepEqual(done.args, { length: 4 });
  assert.deepEqual([...done.result], [1, 2, 3, 4]);
});

test('a digest and an hmac take the data as the body, a string as UTF-8, and the key as base64', async () => {
  const digest = await sent('crypto.digest', () => crypto.digest('sha-256', 'héllo'), { bytes: bytes(9) });
  assert.deepEqual(digest.args, { algorithm: 'sha-256' });
  assert.deepEqual([...digest.body], [...new TextEncoder().encode('héllo')]);
  assert.deepEqual([...digest.result], [9]);
  const raw = await sent('crypto.digest', () => crypto.digest('sha-1', bytes(0, 255)), { bytes: bytes(1) });
  assert.deepEqual(raw.args, { algorithm: 'sha-1' });
  assert.deepEqual([...raw.body], [0, 255]);

  const tag = await sent('crypto.hmac', () => crypto.hmac('sha-512', bytes(1, 2, 3), 'data'), { bytes: bytes(7, 7) });
  assert.deepEqual(tag.args, { algorithm: 'sha-512', key: b64('\x01\x02\x03') });
  assert.deepEqual([...tag.result], [7, 7]);

  const yes = await sent('crypto.hmacVerify', () => crypto.hmacVerify('sha-256', bytes(1), 'data', bytes(5, 6)), { json: { valid: true } });
  assert.deepEqual(yes.args, { algorithm: 'sha-256', key: b64('\x01'), tag: b64('\x05\x06') });
  assert.equal(yes.result, true);
  const no = await sent('crypto.hmacVerify', () => crypto.hmacVerify('sha-256', bytes(1), 'data', bytes(5, 6)), { json: { valid: false } });
  assert.equal(no.result, false);
});

test('the keys derived: what is not given is not sent, what is given goes as base64', async () => {
  const bare = await sent('crypto.hkdf', () => crypto.hkdf('sha-256', bytes(1), { length: 42 }), { bytes: bytes(1) });
  assert.deepEqual(bare.args, { algorithm: 'sha-256', length: 42 });
  const full = await sent('crypto.hkdf', () => crypto.hkdf('sha-384', 'secret', { salt: bytes(1, 2), info: bytes(3), length: 8 }), { bytes: bytes(1) });
  assert.deepEqual(full.args, { algorithm: 'sha-384', salt: b64('\x01\x02'), info: b64('\x03'), length: 8 });
  assert.deepEqual([...full.body], [...new TextEncoder().encode('secret')]);

  const pbkdf2 = await sent('crypto.pbkdf2', () => crypto.pbkdf2('sha-256', 'password', bytes(1, 2, 3), 1000, 32), { bytes: bytes(1) });
  assert.deepEqual(pbkdf2.args, { algorithm: 'sha-256', salt: b64('\x01\x02\x03'), iterations: 1000, length: 32 });

  const argon = await sent('crypto.argon2id', () => crypto.argon2id('password', bytes(1, 2, 3, 4, 5, 6, 7, 8)), { bytes: bytes(1) });
  assert.deepEqual(argon.args, { salt: b64('\x01\x02\x03\x04\x05\x06\x07\x08') });
  const tuned = await sent(
    'crypto.argon2id',
    () => crypto.argon2id(bytes(65), bytes(1, 2, 3, 4, 5, 6, 7, 8), { memoryKiB: 64, iterations: 3, parallelism: 2, length: 24 }),
    { bytes: bytes(1) },
  );
  assert.deepEqual(tuned.args, { salt: b64('\x01\x02\x03\x04\x05\x06\x07\x08'), memoryKiB: 64, iterations: 3, parallelism: 2, length: 24 });
  assert.deepEqual([...tuned.body], [65]);

  const scrypt = await sent('crypto.scrypt', () => crypto.scrypt('password', bytes(1), { logN: 10, r: 8, p: 2, length: 64 }), { bytes: bytes(1) });
  assert.deepEqual(scrypt.args, { salt: b64('\x01'), logN: 10, r: 8, p: 2, length: 64 });
});

test('seal and open: the key and the aad go as base64, the message as the body', async () => {
  const key = new Uint8Array(32).fill(7);
  const sealed = await sent('crypto.seal', () => crypto.seal('aes-256-gcm', key, 'text'), { bytes: bytes(1, 2, 3) });
  assert.deepEqual(sealed.args, { algorithm: 'aes-256-gcm', key: Buffer.from(key).toString('base64') });
  assert.deepEqual([...sealed.body], [...new TextEncoder().encode('text')]);
  const withHeader = await sent('crypto.seal', () => crypto.seal('chacha20-poly1305', key, bytes(1), { aad: bytes(9, 9) }), { bytes: bytes(1) });
  assert.deepEqual(withHeader.args, { algorithm: 'chacha20-poly1305', key: Buffer.from(key).toString('base64'), aad: b64('\x09\x09') });

  const opened = await sent('crypto.open', () => crypto.open('aes-128-gcm', new Uint8Array(16), bytes(5, 5, 5), { aad: bytes(1) }), { bytes: bytes(8) });
  assert.deepEqual(opened.args, { algorithm: 'aes-128-gcm', key: Buffer.from(new Uint8Array(16)).toString('base64'), aad: b64('\x01') });
  assert.deepEqual([...opened.body], [5, 5, 5]);
  assert.deepEqual([...opened.result], [8]);

  replies.set('crypto.open', { status: 400, json: { code: 'INVALID_ARGUMENT', message: 'the message did not pass authentication' } });
  await assert.rejects(crypto.open('aes-256-gcm', key, bytes(1)), error => error instanceof AlefError
    && error.code === 'INVALID_ARGUMENT' && /authentication/.test(error.message));
});

test('an Ed25519 pair comes as bytes, and a signature is made and checked', async () => {
  const seed = Buffer.alloc(32, 1);
  const key = Buffer.alloc(32, 2);
  const made = await sent('crypto.ed25519Generate', () => crypto.ed25519Generate(), {
    json: { privateKey: seed.toString('base64'), publicKey: key.toString('base64') },
  });
  assert.equal(made.args, null);
  assert.ok(made.result.privateKey instanceof Uint8Array);
  assert.deepEqual([...made.result.privateKey], [...seed]);
  assert.deepEqual([...made.result.publicKey], [...key]);

  const signature = await sent('crypto.ed25519Sign', () => crypto.ed25519Sign(made.result.privateKey, 'message'), { bytes: new Uint8Array(64) });
  assert.deepEqual(signature.args, { privateKey: seed.toString('base64') });
  assert.deepEqual([...signature.body], [...new TextEncoder().encode('message')]);
  assert.equal(signature.result.length, 64);

  const verified = await sent('crypto.ed25519Verify', () => crypto.ed25519Verify(made.result.publicKey, 'message', signature.result), { json: { valid: true } });
  assert.deepEqual(verified.args, { publicKey: key.toString('base64'), signature: Buffer.alloc(64).toString('base64') });
  assert.equal(verified.result, true);
});

test('the signal reaches fetch in every command', async () => {
  const controller = new AbortController();
  controller.abort();
  const { signal } = controller;
  const key = new Uint8Array(32);
  const salt = new Uint8Array(8);
  const calls = {
    random: () => crypto.random(1, { signal }),
    digest: () => crypto.digest('sha-256', 'x', { signal }),
    hmac: () => crypto.hmac('sha-256', key, 'x', { signal }),
    hmacVerify: () => crypto.hmacVerify('sha-256', key, 'x', key, { signal }),
    hkdf: () => crypto.hkdf('sha-256', 'x', { length: 8, signal }),
    pbkdf2: () => crypto.pbkdf2('sha-256', 'x', salt, 1, 8, { signal }),
    argon2id: () => crypto.argon2id('x', salt, { signal }),
    scrypt: () => crypto.scrypt('x', salt, { signal }),
    seal: () => crypto.seal('aes-256-gcm', key, 'x', { signal }),
    open: () => crypto.open('aes-256-gcm', key, new Uint8Array(40), { signal }),
    ed25519Generate: () => crypto.ed25519Generate({ signal }),
    ed25519Sign: () => crypto.ed25519Sign(key, 'x', { signal }),
    ed25519Verify: () => crypto.ed25519Verify(key, 'x', new Uint8Array(64), { signal }),
  };
  for (const [name, call] of Object.entries(calls)) {
    await assert.rejects(call(), { name: 'AbortError' }, name);
  }
});
