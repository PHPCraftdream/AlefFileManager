// SPDX-License-Identifier: MIT OR Apache-2.0
import assert from 'node:assert/strict';
import test from 'node:test';
import { AlefError, app } from '../../src/index.ts';
import { installRuntime, jsonFrame, liveStream } from '../fake-runtime.mjs';

const feed = liveStream();
const replies = new Map();
const runtime = installRuntime({
  handler(request) {
    if (request.url === 'native://call/runtime.events.subscribe') return { json: { stream: 5 } };
    if (request.url === 'native://stream/5') return feed.reply;
    return replies.get(request.url.replace('native://call/', '')) ?? { json: null };
  },
});

const emit = (name, payload) => feed.push(jsonFrame({ name, payload }));
const settle = () => new Promise(resolve => setTimeout(resolve, 30));
const argsOf = command => runtime.calls(command).map(call => runtime.argsOf(call));

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
