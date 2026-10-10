// SPDX-License-Identifier: MIT OR Apache-2.0
import assert from 'node:assert/strict';
import test from 'node:test';
import { AlefError, AppWindow, menu, on } from '../../src/index.ts';
import { installRuntime, jsonFrame, liveStream } from '../fake-runtime.mjs';

const feed = liveStream();
let reply = null;
let failure;
let releaseSubscription;
const subscriptionGate = new Promise(resolve => { releaseSubscription = resolve; });
const runtime = installRuntime({
  async handler(request) {
    if (request.url === 'native://call/runtime.events.subscribe') {
      await subscriptionGate;
      return { json: { stream: 5 } };
    }
    if (request.url === 'native://stream/5') return feed.reply;
    if (request.url.startsWith('native://call/menu.')) return failure ?? { json: reply };
    return undefined;
  },
});
const emit = (payload, name = 'runtime.menu.clicked') => feed.push(jsonFrame({ name, payload }));
const items = [
  { kind: 'submenu', label: 'File', items: [
    { id: 'open', label: 'Open', accelerator: 'Ctrl+O', enabled: true },
    { kind: 'check', id: 'hidden', label: 'Hidden', checked: true },
    { kind: 'separator' },
    { role: 'quit', label: 'Quit' },
  ] },
];

// Both predicate waits and promise waits fail within a fixed deadline, including readiness.
async function bounded(promise) {
  let timer;
  try {
    return await Promise.race([promise, new Promise((_, reject) => {
      timer = setTimeout(() => reject(new Error('menu test wait timed out')), 2000);
    })]);
  } finally {
    clearTimeout(timer);
  }
}
async function until(predicate) {
  for (let attempt = 0; attempt < 100; attempt++) {
    if (predicate()) return;
    await new Promise(resolve => setTimeout(resolve, 10));
  }
  assert.fail('menu test predicate timed out');
}
function exactRequest(name, body, signal) {
  const request = runtime.calls(name).at(-1);
  assert.equal(request.method, 'POST');
  assert.equal(request.body, JSON.stringify(body));
  assert.equal(request.headers['content-type'], 'application/json');
  assert.equal(request.headers['x-alef-args'], undefined);
  assert.equal(request.signal, signal);
}

test('menu setters preserve items, serialize only the window label and propagate signals', { timeout: 5000 }, async () => {
  const controller = new AbortController();
  await bounded(menu.setApplicationMenu(items, { signal: controller.signal }));
  exactRequest('menu.setApplicationMenu', { items }, controller.signal);
  await bounded(menu.setWindowMenu(new AppWindow('secondary'), items, { signal: controller.signal }));
  exactRequest('menu.setWindowMenu', { label: 'secondary', items }, controller.signal);
  await bounded(menu.setApplicationMenu([]));
  exactRequest('menu.setApplicationMenu', { items: [] }, undefined);
});

test('popup preserves selected ids and null replies; coordinates stay out of transport options', { timeout: 5000 }, async () => {
  const controller = new AbortController();
  reply = 'open';
  assert.equal(await bounded(menu.popup(items, { x: 0, y: -12.5, signal: controller.signal })), 'open');
  exactRequest('menu.popup', { items, x: 0, y: -12.5 }, controller.signal);
  reply = null;
  assert.equal(await bounded(menu.popup([])), null);
  exactRequest('menu.popup', { items: [] }, undefined);
  await bounded(menu.popup(items, { x: 8 }));
  exactRequest('menu.popup', { items, x: 8 }, undefined);
});

test('all menu calls propagate host errors, including unsupported native features', { timeout: 5000 }, async () => {
  for (const code of ['PERMISSION_DENIED', 'NOT_AVAILABLE', 'INTERNAL']) {
    failure = { status: 500, json: { code, message: 'host menu failure' } };
    try {
      for (const operation of [
        () => menu.setApplicationMenu(items),
        () => menu.setWindowMenu(new AppWindow('main'), items),
        () => menu.popup(items),
      ]) {
        await bounded(assert.rejects(operation(), error => error instanceof AlefError
          && error.code === code && error.message === 'host menu failure'));
      }
    } finally {
      failure = undefined;
    }
  }
});

test('preaborted calls and listeners reject; signals reach calls and listeners do not subscribe', { timeout: 5000 }, async () => {
  const options = { signal: AbortSignal.abort() };
  const before = runtime.calls('runtime.events.subscribe').length;
  for (const operation of [
    () => menu.setApplicationMenu(items, options),
    () => menu.setWindowMenu(new AppWindow('main'), items, options),
    () => menu.popup(items, options),
    () => menu.on('click', () => {}, options),
  ]) await bounded(assert.rejects(operation(), { name: 'AbortError' }));
  assert.equal(runtime.calls('runtime.events.subscribe').length, before);
  for (const name of ['menu.setApplicationMenu', 'menu.setWindowMenu', 'menu.popup']) {
    assert.equal(runtime.calls(name).at(-1).signal, options.signal);
  }
});

test('unknown menu event names reject asynchronously without subscribing', { timeout: 5000 }, async () => {
  const before = runtime.requests.length;
  for (const event of ['', 'clicked', 'unknown']) {
    const result = menu.on(event, () => {});
    assert.equal(typeof result.then, 'function');
    await bounded(assert.rejects(result, { code: 'INVALID_ARGUMENT' }));
  }
  assert.equal(runtime.requests.length, before);
});

test('click listeners await shared readiness, skip malformed payloads and unlisten independently', { timeout: 10000 }, async t => {
  const errors = t.mock.method(console, 'error', () => {});
  const received = [];
  const second = [];
  const controller = new AbortController();
  let ready = false;
  const firstPending = menu.on('click', payload => received.push(payload)).then(off => {
    ready = true;
    return off;
  });
  const secondPending = menu.on('click', payload => second.push(payload), { signal: controller.signal });
  const sharedPending = on('menu.test.barrier', () => {});
  await until(() => runtime.calls('runtime.events.subscribe').length === 1);
  assert.equal(ready, false);
  emit({ id: 'opening' });
  releaseSubscription();
  const [off, stopSecond, stopShared] = await bounded(Promise.all([firstPending, secondPending, sharedPending]));
  await until(() => received.length === 1 && second.length === 1);
  assert.deepEqual(received, [{ id: 'opening' }]);
  assert.equal(runtime.calls('runtime.events.subscribe').length, 1);

  for (const payload of [null, 42, 'open', [], {}, { id: null }, { id: 7 }, { id: '' }, { id: false }]) emit(payload);
  emit({ id: 'wrong-name' }, 'runtime.menu.click');
  emit({ id: 'open', ignored: true });
  await until(() => received.length === 2 && second.length === 2);
  assert.deepEqual(received, [{ id: 'opening' }, { id: 'open' }]);
  assert.equal(errors.mock.callCount(), 0, 'malformed payloads are skipped without throwing');

  off();
  off();
  emit({ id: 'second-only' });
  await until(() => second.length === 3);
  assert.equal(received.length, 2);
  controller.abort();
  stopSecond();
  let barrierSeen = false;
  const stopBarrier = await bounded(on('menu.test.barrier', () => { barrierSeen = true; }));
  emit({ id: 'after-abort' });
  emit({}, 'menu.test.barrier');
  await until(() => barrierSeen);
  assert.equal(second.length, 3);
  assert.equal(runtime.calls('runtime.events.subscribe').length, 1);
  stopBarrier();
  stopShared();
});
