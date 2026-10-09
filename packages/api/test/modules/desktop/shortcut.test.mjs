// SPDX-License-Identifier: MIT OR Apache-2.0
import assert from 'node:assert/strict';
import test from 'node:test';
import { AlefError, Shortcut, shortcut } from '../../../src/index.ts';
import { installRuntime, jsonFrame, liveStream, quiet } from '../../fake-runtime.mjs';

const feed = liveStream();
let reply = { id: 's31:r8', owner: 31, token: 17 };
let failure;
let duringSubscribe;
const runtime = installRuntime({
  handler(request) {
    if (request.url === 'native://call/runtime.events.subscribe') {
      duringSubscribe?.();
      return { json: { stream: 5 } };
    }
    if (request.url === 'native://stream/5') return feed.reply;
    if (request.url === 'native://call/shortcut.register') return failure ?? { json: reply };
    if (request.url === 'native://call/shortcut.unregister') return failure ?? { json: null };
    return undefined;
  },
});
const emit = (payload, name = 'runtime.shortcut.pressed') => feed.push(jsonFrame({ name, payload }));

test('register returns a Shortcut and sends only the accelerator with its signal', async () => {
  const controller = new AbortController();
  const handle = await shortcut.register('CommandOrControl+Shift+K', { signal: controller.signal });
  assert.ok(handle instanceof Shortcut);
  assert.equal(handle.id, 's31:r8');
  assert.deepEqual(Object.keys(handle), ['id']);
  const request = runtime.calls('shortcut.register').at(-1);
  assert.equal(request.method, 'POST');
  assert.equal(request.body, JSON.stringify({ accelerator: 'CommandOrControl+Shift+K' }));
  assert.equal(request.headers['content-type'], 'application/json');
  assert.equal(request.headers['x-alef-args'], undefined);
  assert.equal(request.signal, controller.signal);
});

test('substitution has null token and never subscribes or delivers events', async () => {
  reply = { id: 's31:r9', owner: 31, token: null };
  const handle = await shortcut.register('Ctrl+K');
  let count = 0;
  const before = runtime.calls('runtime.events.subscribe').length;
  const off = await handle.on('pressed', () => count++);
  assert.equal(typeof off, 'function');
  assert.equal(runtime.calls('runtime.events.subscribe').length, before);
  emit({ owner: 31, token: null });
  emit({ owner: 31, token: 999 });
  off();
  await handle.unregister();
  assert.equal(count, 0);
  assert.deepEqual(runtime.argsOf(runtime.calls('shortcut.unregister').at(-1)), { id: 's31:r9' });
});

test('shared subscription catches opening-race events and filters both owner and token', async t => {
  const failures = t.mock.method(console, 'error', () => {});
  reply = { id: 's31:r8', owner: 31, token: 17 };
  const handle = await shortcut.register('Ctrl+K');
  let count = 0;
  duringSubscribe = () => emit({ owner: 31, token: 17 });
  const off = await handle.on('pressed', () => count++);
  duringSubscribe = undefined;
  await quiet(runtime);
  assert.equal(count, 1, 'handler is installed before the shared stream opens');
  for (const payload of [null, 17, {}, { owner: 32, token: 17 }, { owner: 31, token: 18 }, { owner: '31', token: 17 }, { owner: 31, token: '17' }]) emit(payload);
  emit({ owner: 31, token: 17 }, 'runtime.shortcut.released');
  emit({ owner: 31, token: 17 }, 'runtime.shortcut.unknown');
  emit({ owner: 31, token: 17 });
  await quiet(runtime);
  assert.equal(count, 2);
  assert.equal(failures.mock.callCount(), 0, 'a payload of the wrong shape is skipped, not a failure of the handler');
  assert.equal(runtime.calls('runtime.events.subscribe').length, 1);
  off();
  emit({ owner: 31, token: 17 });
  await quiet(runtime);
  assert.equal(count, 2);
});

test('listener abort and successful unregister stop delivery; unregister preserves body and signal', async () => {
  const handle = await shortcut.register('Ctrl+K');
  let count = 0;
  const controller = new AbortController();
  const off = await handle.on('pressed', () => count++, { signal: controller.signal });
  controller.abort();
  emit({ owner: 31, token: 17 });
  await quiet(runtime);
  assert.equal(count, 0);
  off();
  const stop = await handle.on('pressed', () => count++);
  const unregisterController = new AbortController();
  await handle.unregister({ signal: unregisterController.signal });
  const request = runtime.calls('shortcut.unregister').at(-1);
  assert.equal(request.body, JSON.stringify({ id: 's31:r8' }));
  assert.equal(request.headers['content-type'], 'application/json');
  assert.equal(request.headers['x-alef-args'], undefined);
  assert.equal(request.signal, unregisterController.signal);
  emit({ owner: 31, token: 17 });
  await quiet(runtime);
  assert.equal(count, 0);
  stop();
});

test('register and unregister errors propagate; failed unregister keeps event delivery for retry', async () => {
  failure = { status: 403, json: { code: 'PERMISSION_DENIED', message: 'denied' } };
  try {
    await assert.rejects(shortcut.register('Ctrl+K'), error => error instanceof AlefError && error.code === 'PERMISSION_DENIED');
  } finally {
    failure = undefined;
  }
  const handle = await shortcut.register('Ctrl+K');
  let count = 0;
  const off = await handle.on('pressed', () => count++);
  failure = { status: 500, json: { code: 'INTERNAL', message: 'host failed' } };
  try {
    await assert.rejects(handle.unregister(), { code: 'INTERNAL', message: 'host failed' });
  } finally {
    failure = undefined;
  }
  emit({ owner: 31, token: 17 });
  await quiet(runtime);
  assert.equal(count, 1);
  await handle.unregister();
  off();
});

test('already-aborted signals reject registration, unregister and listeners including substitutes', async () => {
  const handle = await shortcut.register('Ctrl+K');
  const controller = new AbortController();
  controller.abort();
  const options = { signal: controller.signal };
  await assert.rejects(shortcut.register('Ctrl+K', options), { name: 'AbortError' });
  await assert.rejects(handle.unregister(options), { name: 'AbortError' });
  await assert.rejects(handle.on('pressed', () => {}, options), { name: 'AbortError' });
  reply = { id: 's31:r10', owner: 31, token: null };
  const substitute = await shortcut.register('Ctrl+L');
  await assert.rejects(substitute.on('pressed', () => {}, options), { name: 'AbortError' });
  reply = { id: 's31:r8', owner: 31, token: 17 };
});

test('unknown event names reject at runtime without opening a subscription', async () => {
  for (const token of [17, null]) {
    const handle = new Shortcut({ id: 's31:r8', owner: 31, token });
    const before = runtime.requests.length;
    await assert.rejects(handle.on('released', () => {}), { code: 'INVALID_ARGUMENT' });
    await assert.rejects(handle.on('', () => {}), { code: 'INVALID_ARGUMENT' });
    assert.equal(runtime.requests.length, before);
  }
});

test('async disposal unregisters where the symbol is available', async () => {
  if (typeof Symbol.asyncDispose !== 'symbol') return;
  const handle = await shortcut.register('Ctrl+K');
  await handle[Symbol.asyncDispose]();
  assert.deepEqual(runtime.argsOf(runtime.calls('shortcut.unregister').at(-1)), { id: handle.id });
});
