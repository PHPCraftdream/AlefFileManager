// SPDX-License-Identifier: MIT OR Apache-2.0
import assert from 'node:assert/strict';
import test from 'node:test';
import { AlefError, app } from '../../../src/index.ts';
import { installRuntime, jsonFrame, liveStream, quiet } from '../../fake-runtime.mjs';

const feed = liveStream();
const replies = new Map();
const callWaiters = new Set();
const runtime = installRuntime({
  handler(request) {
    for (const waiter of [...callWaiters]) waiter(request);
    if (request.url === 'native://call/runtime.events.subscribe') return { json: { stream: 5 } };
    if (request.url === 'native://stream/5') return feed.reply;
    return replies.get(request.url.replace('native://call/', '')) ?? { json: null };
  },
});

const emit = (name, payload) => feed.push(jsonFrame({ name, payload }));
const settle = () => quiet(runtime);
const argsOf = command => runtime.calls(command).map(call => runtime.argsOf(call));

// Deadline timers only bound missing handshakes; no polling or scheduling sleeps.
function handshake(label) {
  let resolve;
  let reject;
  const promise = new Promise((yes, no) => { resolve = yes; reject = no; });
  const timer = setTimeout(() => reject(new Error(`missing ${label}`)), 2000);
  return { promise: promise.finally(() => clearTimeout(timer)), resolve };
}
function disabled() {
  const done = handshake('open-url disable call');
  const waiter = request => {
    if (request.url === 'native://call/app.openUrlIntercept' && !runtime.argsOf(request).enabled) done.resolve();
  };
  callWaiters.add(waiter);
  return done.promise.finally(() => callWaiters.delete(waiter));
}
function leases() {
  const active = new Set();
  return {
    own(off) {
      const stop = () => { active.delete(stop); off(); };
      active.add(stop);
      return stop;
    },
    async close() {
      if (!active.size) return;
      const done = disabled();
      for (const stop of active) stop();
      await done;
    },
  };
}

test('open-url explicitly enables delivery even when another listener subscribed first', { timeout: 3000 }, async () => {
  const owned = leases();
  let other;
  try {
    other = await app.on('second-instance', () => {});
    const before = argsOf('app.openUrlIntercept').length;
    const heard = [];
    const one = owned.own(await app.on('open-url', ({ url }) => heard.push(['one', url])));
    let delivered;
    const two = owned.own(await app.on('open-url', ({ url }) => {
      heard.push(['two', url]);
      delivered.resolve();
    }));
    assert.deepEqual(argsOf('app.openUrlIntercept').slice(before), [{ enabled: true }]);
    delivered = handshake('both open-url callbacks');
    emit('app.open-url', { url: 'alef:startup' });
    await delivered.promise;
    assert.deepEqual(heard, [['one', 'alef:startup'], ['two', 'alef:startup']]);
    one();
    // Registration completion drains the queued non-last unlisten deterministically.
    const barrier = owned.own(await app.on('open-url', () => {}));
    barrier();
    assert.deepEqual(argsOf('app.openUrlIntercept').slice(before), [{ enabled: true }]);
    const done = disabled();
    two();
    two();
    await done;
    assert.deepEqual(argsOf('app.openUrlIntercept').slice(before), [{ enabled: true }, { enabled: false }]);
  } finally {
    other?.();
    await owned.close();
  }
});

test('open-url last unlisten cannot disable a newly added subscription', { timeout: 3000 }, async () => {
  const owned = leases();
  try {
    const before = argsOf('app.openUrlIntercept').length;
    const first = owned.own(await app.on('open-url', () => {}));
    first();
    const heard = [];
    let delivered;
    owned.own(await app.on('open-url', ({ url }) => { heard.push(url); delivered.resolve(); }));
    // Registration completes after the queued last-unlisten, without yielding to a timer.
    assert.deepEqual(argsOf('app.openUrlIntercept').slice(before), [{ enabled: true }]);
    delivered = handshake('replacement open-url callback');
    emit('app.open-url', { url: 'alef:new' });
    await delivered.promise;
    assert.deepEqual(heard, ['alef:new']);
  } finally {
    await owned.close();
  }
});

test('open-url abort cleanup and duplicate callback leases are independent', { timeout: 3000 }, async () => {
  const owned = leases();
  const controller = new AbortController();
  try {
    const heard = [];
    let delivered;
    const handler = ({ url }) => { heard.push(url); delivered.resolve(); };
    const first = owned.own(await app.on('open-url', handler, { signal: controller.signal }));
    owned.own(await app.on('open-url', handler));
    controller.abort();
    first();
    delivered = handshake('remaining callback lease');
    emit('app.open-url', { url: 'alef:once' });
    await delivered.promise;
    assert.deepEqual(heard, ['alef:once']);
    await assert.rejects(app.on('open-url', handler, { signal: controller.signal }), { name: 'AbortError' });
  } finally {
    controller.abort();
    await owned.close();
  }
});

test('open-url failed enable removes handler and subscription; retry succeeds', { timeout: 3000 }, async () => {
  const owned = leases();
  try {
    const heard = [];
    replies.set('app.openUrlIntercept', { status: 501, json: { code: 'NOT_AVAILABLE', message: 'no' } });
    await assert.rejects(app.on('open-url', ({ url }) => heard.push(['failed', url])), error => error instanceof AlefError);
    replies.delete('app.openUrlIntercept');
    let delivered;
    owned.own(await app.on('open-url', ({ url }) => { heard.push(['live', url]); delivered.resolve(); }));
    delivered = handshake('retry open-url callback');
    emit('app.open-url', { url: 'alef:retry' });
    await delivered.promise;
    assert.deepEqual(heard, [['live', 'alef:retry']]);
  } finally {
    replies.delete('app.openUrlIntercept');
    await owned.close();
  }
});

test('open-url with an aborted signal asks for nothing', { timeout: 3000 }, async () => {
  const before = argsOf('app.openUrlIntercept').length;
  const controller = new AbortController();
  controller.abort();
  await assert.rejects(app.on('open-url', () => {}, { signal: controller.signal }), { name: 'AbortError' });
  await settle();
  assert.equal(argsOf('app.openUrlIntercept').length, before);
});

test('open-url aborted while the subscription is made leaves nothing behind', { timeout: 3000 }, async () => {
  const owned = leases();
  const controller = new AbortController();
  try {
    const before = argsOf('app.openUrlIntercept').length;
    const pending = app.on('open-url', () => {}, { signal: controller.signal });
    // Runs after the queued subscription starts and before it finishes.
    queueMicrotask(() => controller.abort());
    await assert.rejects(pending, { name: 'AbortError' });
    await settle();
    assert.deepEqual(argsOf('app.openUrlIntercept').slice(before), []);
    let delivered;
    owned.own(await app.on('open-url', () => delivered.resolve()));
    assert.deepEqual(argsOf('app.openUrlIntercept').slice(before), [{ enabled: true }]);
    delivered = handshake('open-url after the aborted subscription');
    emit('app.open-url', { url: 'alef:later' });
    await delivered.promise;
  } finally {
    await owned.close();
  }
});

test('open-url: a failing callback does not keep the next one from the url', { timeout: 3000 }, async () => {
  const owned = leases();
  const logged = [];
  const original = console.error;
  console.error = (...args) => logged.push(args);
  try {
    const heard = [];
    let delivered;
    owned.own(await app.on('open-url', () => { throw new Error('refused'); }));
    owned.own(await app.on('open-url', ({ url }) => { heard.push(url); delivered.resolve(); }));
    delivered = handshake('callback after the failing one');
    emit('app.open-url', { url: 'alef:after-failure' });
    await delivered.promise;
    assert.deepEqual(heard, ['alef:after-failure']);
    assert.equal(logged.length, 1);
  } finally {
    console.error = original;
    await owned.close();
  }
});

test('requestSingleInstance sends no arguments and gives the answer back', async () => {
  replies.set('app.requestSingleInstance', { json: true });
  assert.equal(await app.requestSingleInstance(), true);
  replies.set('app.requestSingleInstance', { json: false });
  assert.equal(await app.requestSingleInstance(), false);
  assert.deepEqual(argsOf('app.requestSingleInstance'), [null, null]);
  replies.set('app.requestSingleInstance', { status: 501, json: { code: 'NOT_AVAILABLE', message: 'single instance: no endpoint' } });
  await assert.rejects(app.requestSingleInstance(), error => error instanceof AlefError && error.code === 'NOT_AVAILABLE');
});

test('second-instance hands the command line and the working directory to the handler', async () => {
  const heard = [];
  const off = await app.on('second-instance', info => heard.push(info));
  const info = { args: { raw: ['--port', '2', 'b.txt'], parsed: { port: 2 }, positional: ['b.txt'] }, cwd: '/work' };
  emit('app.second-instance', info);
  await settle();
  assert.deepEqual(heard, [info]);
  off();
  emit('app.second-instance', info);
  await settle();
  assert.equal(heard.length, 1, 'a handler that was removed hears nothing');
  assert.equal(argsOf('app.quitIntercept').length, 0, 'second-instance asks nothing of the runtime');
});

test('an event nobody knows is refused', async () => {
  await assert.rejects(app.on('quit-ish', () => {}), error => error instanceof AlefError && error.code === 'INVALID_ARGUMENT');
});

test('before-quit: the first handler turns interception on, a handler decides, the answer names the question', async () => {
  const before = argsOf('app.quitIntercept').length;
  const asked = [];
  const off = await app.on('before-quit', event => {
    asked.push(event.defaultPrevented);
    event.preventDefault();
  });
  assert.deepEqual(argsOf('app.quitIntercept').slice(before), [{ enabled: true }]);
  const answers = argsOf('app.quitAnswer').length;
  emit('app.before-quit', { id: 7 });
  await settle();
  assert.deepEqual(asked, [false]);
  assert.deepEqual(argsOf('app.quitAnswer').slice(answers), [{ id: 7, prevent: true }]);
  off();
  await settle();
  assert.deepEqual(argsOf('app.quitIntercept').slice(before), [{ enabled: true }, { enabled: false }], 'the last handler lifts interception');
});

test('before-quit: no handler objects means the quit is allowed, any handler can veto, async ones are awaited', async () => {
  const answers = argsOf('app.quitAnswer').length;
  const offs = [];
  offs.push(await app.on('before-quit', () => {}));
  emit('app.before-quit', { id: 1 });
  await settle();
  offs.push(await app.on('before-quit', async event => {
    await new Promise(resolve => setTimeout(resolve, 20));
    event.preventDefault();
  }));
  emit('app.before-quit', { id: 2 });
  await settle();
  await settle();
  const seen = argsOf('app.quitAnswer').slice(answers);
  assert.deepEqual(seen, [{ id: 1, prevent: false }, { id: 2, prevent: true }]);
  for (const off of offs) off();
  await settle();
});

test('before-quit: a handler that throws does not stop the others or the answer', async () => {
  const errors = [];
  const original = console.error;
  console.error = (...parts) => errors.push(parts.join(' '));
  const answers = argsOf('app.quitAnswer').length;
  try {
    const first = await app.on('before-quit', () => { throw new Error('boom'); });
    const second = await app.on('before-quit', event => event.preventDefault());
    emit('app.before-quit', { id: 9 });
    await settle();
    assert.deepEqual(argsOf('app.quitAnswer').slice(answers), [{ id: 9, prevent: true }]);
    assert.ok(errors.some(text => text.includes('before-quit handler failed')), errors.join('|'));
    first();
    second();
    await settle();
  } finally {
    console.error = original;
  }
});

test('before-quit: aborting the signal stops the handler', async () => {
  const controller = new AbortController();
  const handled = [];
  await app.on('before-quit', () => handled.push(1), { signal: controller.signal });
  controller.abort();
  await settle();
  emit('app.before-quit', { id: 11 });
  await settle();
  assert.deepEqual(handled, []);
});

test('before-quit: when interception cannot be turned on the handler is not kept', async () => {
  replies.set('app.quitIntercept', { status: 403, json: { code: 'PERMISSION_DENIED', message: 'no' } });
  await assert.rejects(app.on('before-quit', () => {}), error => error instanceof AlefError);
  replies.delete('app.quitIntercept');
  const handled = [];
  const off = await app.on('before-quit', () => handled.push(1));
  emit('app.before-quit', { id: 12 });
  await settle();
  assert.deepEqual(handled, [1], 'the failed handler left nothing behind and a new one works');
  off();
  await settle();
});
