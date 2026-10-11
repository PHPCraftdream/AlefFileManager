// SPDX-License-Identifier: MIT OR Apache-2.0
import assert from 'node:assert/strict';
import test from 'node:test';
import { AlefError, on } from '../src/index.ts';
import { barrier, installRuntime, jsonFrame, liveStream } from './fake-runtime.mjs';

const feed = liveStream();
let readGate;
let readEntered;
const body = feed.reply.body.pipeThrough(new TransformStream({
  async transform(bytes, controller) {
    if (readGate) {
      readEntered();
      await readGate;
    }
    controller.enqueue(bytes);
  },
}));
let subscribeStatus = 200;
const runtime = installRuntime({
  handler(request) {
    if (request.url === 'native://call/runtime.events.subscribe') {
      return subscribeStatus === 200
        ? { json: { stream: 5 } }
        : { status: subscribeStatus, json: { code: 'INTERNAL', message: 'subscribe failed' } };
    }
    if (request.url === 'native://stream/5') return { ...feed.reply, body };
    return undefined;
  },
});

const emit = (name, payload) => feed.push(jsonFrame({ name, payload }));
const settle = () => barrier(feed, on, 'test.events.marker');

test('a failed subscription rejects, leaves no handler behind and is retried by the next on()', async () => {
  subscribeStatus = 500;
  const seen = [];
  await assert.rejects(on('early', value => seen.push(value)), error => error instanceof AlefError && error.code === 'INTERNAL');
  subscribeStatus = 200;
  const off = await on('late', () => {});
  emit('early', 'must not arrive');
  await settle();
  assert.deepEqual(seen, [], 'the handler of the failed on() was removed');
  off();
});

test('the barrier waits for a blocked read even when the requests of the runtime stay unchanged', { timeout: 3000 }, async () => {
  let release;
  let entered;
  readGate = new Promise(resolve => { release = resolve; });
  let readingTimer;
  const reading = new Promise(resolve => { entered = resolve; });
  readEntered = entered;
  const seen = [];
  let off;
  let completed = false;
  let pending;
  try {
    off = await on('blocked', value => seen.push(value));
    const before = runtime.requests.length;
    emit('blocked', 1);
    pending = settle().then(() => { completed = true; });
    await Promise.race([
      reading,
      new Promise((resolve, reject) => {
        readingTimer = setTimeout(() => reject(new Error('missing blocked read')), 1000);
      }),
    ]);
    clearTimeout(readingTimer);
    // Longer than the five polls of 10 ms that a wait for request silence would need.
    await new Promise(resolve => setTimeout(resolve, 100));
    assert.equal(runtime.requests.length, before, 'request silence cannot establish delivery');
    assert.deepEqual(seen, []);
    assert.equal(completed, false, 'the marker cannot overtake the blocked frame');
    release();
    await pending;
    assert.deepEqual(seen, [1]);
    off();
    emit('blocked', 2);
    await settle();
    assert.deepEqual(seen, [1], 'the ignored frame was read before the marker');
  } finally {
    clearTimeout(readingTimer);
    release();
    readGate = undefined;
    readEntered = undefined;
    off?.();
    await pending;
  }
});

test('the barrier gives up when the stream delivers nothing', { timeout: 3000 }, async () => {
  let release;
  readGate = new Promise(resolve => { release = resolve; });
  readEntered = () => {};
  try {
    await assert.rejects(barrier(feed, on, 'test.events.never', 100), /missing test\.events\.never/);
  } finally {
    release();
    readGate = undefined;
    readEntered = undefined;
  }
  await settle();
});

test('one subscription serves every handler; events are matched by name', async () => {
  const before = runtime.calls('runtime.events.subscribe').length;
  const first = [];
  const second = [];
  const offFirst = await on('backend.greeting', payload => first.push(payload));
  const offSecond = await on('runtime.window.state', payload => second.push(payload));
  assert.equal(runtime.calls('runtime.events.subscribe').length, before, 'the document already has its subscription');

  emit('backend.greeting', { message: 'hi' });
  emit('runtime.window.state', { revision: 2 });
  emit('someone.else', 1);
  await settle();
  assert.deepEqual(first, [{ message: 'hi' }]);
  assert.deepEqual(second, [{ revision: 2 }]);

  offFirst();
  emit('backend.greeting', { message: 'after off' });
  emit('runtime.window.state', { revision: 3 });
  await settle();
  assert.deepEqual(first, [{ message: 'hi' }], 'no delivery after unsubscribing');
  assert.deepEqual(second, [{ revision: 2 }, { revision: 3 }], 'the other handler is unaffected');
  offSecond();
});

test('aborting the signal unsubscribes; an aborted signal or an empty name is refused', async () => {
  const seen = [];
  const controller = new AbortController();
  await on('signalled', payload => seen.push(payload), { signal: controller.signal });
  emit('signalled', 1);
  await settle();
  controller.abort();
  emit('signalled', 2);
  await settle();
  assert.deepEqual(seen, [1]);
  await assert.rejects(on('x', () => {}, { signal: controller.signal }), { name: 'AbortError' });
  await assert.rejects(on('', () => {}), error => error instanceof AlefError && error.code === 'INVALID_ARGUMENT');
});

test('a handler that throws neither stops the others nor the stream', async (t) => {
  t.mock.method(console, 'error', () => {});
  const seen = [];
  const offBad = await on('boom', () => { throw new Error('handler bug'); });
  const offGood = await on('boom', payload => seen.push(payload));
  emit('boom', 1);
  emit('boom', 2);
  await settle();
  assert.deepEqual(seen, [1, 2]);
  offBad();
  offGood();
});
