// SPDX-License-Identifier: MIT OR Apache-2.0
import assert from 'node:assert/strict';
import test from 'node:test';
import { AlefError, Store, store } from '../../../src/index.ts';
import { installRuntime } from '../../fake-runtime.mjs';

const replies = new Map();
const runtime = installRuntime({
  handler(request) {
    return replies.get(request.url.replace('native://call/', '')) ?? { json: null };
  },
});

async function sent(command, fn, reply = { json: null }) {
  replies.set(command, reply);
  const before = runtime.calls(command).length;
  const result = await fn();
  const calls = runtime.calls(command);
  assert.equal(calls.length, before + 1, `${command} was called once`);
  return { result, request: calls.at(-1), args: runtime.argsOf(calls.at(-1)) };
}

test('get gives the value, undefined when the answer has none, and a stored null stays null', async () => {
  const hit = await sent('store.get', () => store.get('k'), { json: { value: { a: [1, 2] } } });
  assert.deepEqual(hit.args, { key: 'k' }, 'the default area is not named');
  assert.deepEqual(hit.result, { a: [1, 2] });
  const none = await sent('store.get', () => store.get('k'), { json: {} });
  assert.equal(none.result, undefined);
  const stored = await sent('store.get', () => store.get('k'), { json: { value: null } });
  assert.equal(stored.result, null);
});

test('set sends the key and the value; undefined is refused before anything is sent', async () => {
  const set = await sent('store.set', () => store.set('k', { n: 1 }));
  assert.deepEqual(set.args, { key: 'k', value: { n: 1 } });
  const falsy = await sent('store.set', () => store.set('k', null));
  assert.deepEqual(falsy.args, { key: 'k', value: null }, 'null is a value');
  const before = runtime.calls('store.set').length;
  await assert.rejects(store.set('k', undefined), error => error instanceof AlefError && error.code === 'INVALID_ARGUMENT');
  assert.equal(runtime.calls('store.set').length, before);
});

test('delete, keys and flush send what they are given', async () => {
  const removed = await sent('store.delete', () => store.delete('k'));
  assert.deepEqual(removed.args, { key: 'k' });
  const all = await sent('store.keys', () => store.keys(), { json: ['a', 'b'] });
  assert.deepEqual(all.args, {});
  assert.deepEqual(all.result, ['a', 'b']);
  const some = await sent('store.keys', () => store.keys('p/'), { json: ['p/1'] });
  assert.deepEqual(some.args, { prefix: 'p/' });
  const flushed = await sent('store.flush', () => store.flush());
  assert.equal(flushed.args, null);
});

test('open asks the runtime about the name; the area sends its name with everything', async () => {
  const opened = await sent('store.open', () => store.open('prefs'));
  assert.deepEqual(opened.args, { area: 'prefs' });
  assert.ok(opened.result instanceof Store);
  assert.equal(opened.result.area, 'prefs');
  const area = opened.result;
  assert.deepEqual((await sent('store.get', () => area.get('k'), { json: { value: 1 } })).args, { area: 'prefs', key: 'k' });
  assert.deepEqual((await sent('store.set', () => area.set('k', 2))).args, { area: 'prefs', key: 'k', value: 2 });
  assert.deepEqual((await sent('store.delete', () => area.delete('k'))).args, { area: 'prefs', key: 'k' });
  assert.deepEqual((await sent('store.keys', () => area.keys('a'), { json: [] })).args, { area: 'prefs', prefix: 'a' });
  assert.deepEqual((await sent('store.keys', () => area.keys(), { json: [] })).args, { area: 'prefs' });
  await sent('store.flush', () => area.flush());

  replies.set('store.open', { status: 400, json: { code: 'INVALID_ARGUMENT', message: 'bad name' } });
  await assert.rejects(store.open('a b'), error => error instanceof AlefError && error.code === 'INVALID_ARGUMENT');
});

test('the signal reaches fetch', async () => {
  const controller = new AbortController();
  controller.abort();
  await assert.rejects(store.get('k', { signal: controller.signal }), { name: 'AbortError' });
});
