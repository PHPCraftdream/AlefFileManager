// SPDX-License-Identifier: MIT OR Apache-2.0
import assert from 'node:assert/strict';
import test from 'node:test';
import { AlefError, secrets } from '../../../src/index.ts';
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

test('get names the secret and gives the bytes, or null when nothing is kept', async () => {
  const hit = await sent('secrets.get', () => secrets.get('mail', 'me'), { bytes: new Uint8Array([1, 2, 3]) });
  assert.deepEqual(hit.args, { service: 'mail', account: 'me' });
  assert.deepEqual([...hit.result], [1, 2, 3]);
  const none = await sent('secrets.get', () => secrets.get('mail', 'me'), { json: null });
  assert.equal(none.result, null);
});

test('getText decodes UTF-8 and keeps null a null', async () => {
  const text = await sent('secrets.get', () => secrets.getText('mail', 'me'), { bytes: new TextEncoder().encode('пароль 🙂') });
  assert.equal(text.result, 'пароль 🙂');
  assert.deepEqual(text.args, { service: 'mail', account: 'me' });
  const none = await sent('secrets.get', () => secrets.getText('mail', 'me'), { json: null });
  assert.equal(none.result, null);
});

test('set sends the names as arguments and the secret, a string as its UTF-8, as the body', async () => {
  const text = await sent('secrets.set', () => secrets.set('mail', 'me', 'пароль'), { json: null });
  assert.deepEqual(text.args, { service: 'mail', account: 'me' });
  assert.deepEqual([...text.body], [...new TextEncoder().encode('пароль')]);
  const raw = await sent('secrets.set', () => secrets.set('chat', 'you', new Uint8Array([0, 255, 7])), { json: null });
  assert.deepEqual(raw.args, { service: 'chat', account: 'you' });
  assert.deepEqual([...raw.body], [0, 255, 7]);
});

test('delete says whether there was a secret', async () => {
  const had = await sent('secrets.delete', () => secrets.delete('mail', 'me'), { json: { deleted: true } });
  assert.deepEqual(had.args, { service: 'mail', account: 'me' });
  assert.equal(had.result, true);
  const hadNot = await sent('secrets.delete', () => secrets.delete('mail', 'me'), { json: { deleted: false } });
  assert.equal(hadNot.result, false);
});

test('a refusal of the runtime reaches the caller as it is', async () => {
  replies.set('secrets.get', { status: 403, json: { code: 'PERMISSION_DENIED', message: 'no', details: { permission: 'secrets' } } });
  await assert.rejects(secrets.get('mail', 'me'), error => error instanceof AlefError
    && error.code === 'PERMISSION_DENIED' && error.details?.permission === 'secrets');
  replies.set('secrets.set', { status: 400, json: { code: 'INVALID_ARGUMENT', message: 'a secret has from 1 to 1024 bytes' } });
  await assert.rejects(secrets.set('mail', 'me', ''), error => error instanceof AlefError && error.code === 'INVALID_ARGUMENT');
});

test('the signal reaches fetch in every command', async () => {
  const controller = new AbortController();
  controller.abort();
  const { signal } = controller;
  const calls = {
    get: () => secrets.get('mail', 'me', { signal }),
    getText: () => secrets.getText('mail', 'me', { signal }),
    set: () => secrets.set('mail', 'me', 'x', { signal }),
    delete: () => secrets.delete('mail', 'me', { signal }),
  };
  for (const [name, call] of Object.entries(calls)) {
    await assert.rejects(call(), { name: 'AbortError' }, name);
  }
});
