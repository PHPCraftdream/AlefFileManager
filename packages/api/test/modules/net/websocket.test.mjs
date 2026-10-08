// SPDX-License-Identifier: MIT OR Apache-2.0
import assert from 'node:assert/strict';
import test from 'node:test';
import { AlefError, websocket, WebSocketConnection } from '../../../src/index.ts';
import { binaryFrame, endFrame, errorFrame, installRuntime, join, jsonFrame } from '../../fake-runtime.mjs';

const replies = new Map();
const streams = new Map();
/** Calls the runtime keeps waiting until their signal is aborted (when they carry one). */
const hold = new Set();
let onHold = () => {};
/** A call that never reaches the runtime (it has no signal to hold on) must not hang the test. */
const soon = promise => Promise.race([promise, new Promise(resolve => setTimeout(resolve, 500))]);
const runtime = installRuntime({
  limits: { chunkSize: 64 * 1024 },
  handler(request) {
    const stream = /^native:\/\/stream\/(\d+)$/.exec(request.url);
    if (stream) return streams.get(Number(stream[1])) ?? { status: 404, json: { code: 'NOT_FOUND', message: 'stream not found' } };
    const name = request.url.replace('native://call/', '');
    if (hold.has(name) && request.signal) {
      onHold();
      return new Promise((_, reject) => request.signal.addEventListener('abort', () => reject(new DOMException('aborted', 'AbortError')), { once: true }));
    }
    return replies.get(name) ?? { json: null };
  },
});

const encode = text => new TextEncoder().encode(text);
const argsOf = command => runtime.argsOf(runtime.calls(command).at(-1));
const opened = (over = {}) => ({ socket: 7, messages: 71, protocol: '', url: 'ws://h.test/chat', ...over });
const closeEvent = (code, reason, clean) => jsonFrame({ type: 'close', code, reason, clean });

async function open(frames, over = {}, options = {}) {
  replies.set('websocket.connect', { json: opened(over) });
  streams.set(over.messages ?? 71, { chunks: [join(...frames)] });
  return websocket.connect('ws://h.test/chat', options);
}

async function collect(connection) {
  const got = [];
  for await (const message of connection) got.push(message.type === 'text' ? message : { type: message.type, data: [...message.data] });
  return got;
}

test('connect names the address, the subprotocols, the headers, the authority and the time', async () => {
  const connection = await open([endFrame()], { protocol: 'superchat' }, {
    protocols: ['chat', 'superchat'], headers: { 'X-One': '1', Origin: 'https://app.test' }, ca: 'PEM', timeout: 900,
  });
  assert.ok(connection instanceof WebSocketConnection);
  assert.deepEqual(argsOf('websocket.connect'), {
    url: 'ws://h.test/chat', protocols: ['chat', 'superchat'], headers: [['origin', 'https://app.test'], ['x-one', '1']], ca: 'PEM', timeoutMs: 900,
  });
  assert.equal(connection.url, 'ws://h.test/chat');
  assert.equal(connection.protocol, 'superchat');

  await open([endFrame()]);
  assert.deepEqual(argsOf('websocket.connect'), { url: 'ws://h.test/chat' }, 'nothing else is asked for when nothing is given');
});

test('the messages come whole as text or bytes, in pieces or none, and the close ends the iteration', async () => {
  const text = encode('héllo');
  const connection = await open([
    jsonFrame({ type: 'text', length: text.length }), binaryFrame(text.subarray(0, 2)), binaryFrame(text.subarray(2)),
    jsonFrame({ type: 'binary', length: 3 }), binaryFrame(new Uint8Array([1, 2, 3])),
    jsonFrame({ type: 'text', length: 0 }),
    jsonFrame({ type: 'binary', length: 0 }),
    jsonFrame({ type: 'text', length: 2 }), binaryFrame(encode('ok')),
    closeEvent(4001, 'bye', true),
    endFrame(),
  ]);
  const got = await collect(connection);
  assert.deepEqual(got, [
    { type: 'text', data: 'héllo' },
    { type: 'binary', data: [1, 2, 3] },
    { type: 'text', data: '' },
    { type: 'binary', data: [] },
    { type: 'text', data: 'ok' },
  ]);
  assert.deepEqual(await connection.closed, { code: 4001, reason: 'bye', clean: true });
});

test('a connection that ends is released in the runtime once, and a close after that is nothing', async () => {
  const connection = await open([closeEvent(1000, '', true), endFrame()], { socket: 8 });
  const before = runtime.calls('websocket.close').length;
  await collect(connection);
  assert.equal(runtime.calls('websocket.close').length, before + 1);
  assert.deepEqual(argsOf('websocket.close'), { socket: 8 });
  await connection.close();
  assert.equal(runtime.calls('websocket.close').length, before + 1, 'closed already');
  assert.deepEqual(await connection.closed, { code: 1000, reason: '', clean: true });

  replies.set('websocket.close', { status: 404, json: { code: 'NOT_FOUND', message: 'gone' } });
  const gone = await open([endFrame()], { socket: 9 });
  assert.deepEqual(await collect(gone), [], 'a release that finds nothing is no failure');
  replies.delete('websocket.close');
});

test('a connection that ends without a close says so, and leaving the iteration early closes the connection', async () => {
  const connection = await open([endFrame()], { socket: 10 });
  await collect(connection);
  assert.deepEqual(await connection.closed, { code: 1006, reason: '', clean: false });

  const early = await open([jsonFrame({ type: 'text', length: 1 }), binaryFrame(encode('a')), jsonFrame({ type: 'text', length: 1 }), binaryFrame(encode('b')), endFrame()], { socket: 11 });
  const before = runtime.calls('websocket.close').length;
  for await (const message of early) {
    assert.equal(message.data, 'a');
    break;
  }
  assert.equal(runtime.calls('websocket.close').length, before + 1);
  assert.deepEqual(argsOf('websocket.close'), { socket: 11 });
});

test('a failure of the stream reaches the one who waits, unless the page closed the connection', async () => {
  const failing = await open([jsonFrame({ type: 'text', length: 1 }), binaryFrame(encode('a')), errorFrame({ code: 'NETWORK', message: 'broke' })], { socket: 12 });
  const got = [];
  await assert.rejects(async () => {
    for await (const message of failing) got.push(message.data);
  }, error => error instanceof AlefError && error.code === 'NETWORK');
  assert.deepEqual(got, ['a']);
  assert.deepEqual(await failing.closed, { code: 1006, reason: '', clean: false });

  const closed = await open([errorFrame({ code: 'CLOSED', message: 'closed' })], { socket: 13 });
  await closed.close();
  assert.deepEqual(await collect(closed), [], 'the failure of a stream the page closed is its end');
});

test('send names the connection and the kind, and a message is at most 192 KiB', async () => {
  const connection = await open([endFrame()], { socket: 14 });
  await connection.send('héllo');
  assert.deepEqual(argsOf('websocket.send'), { socket: 14, text: true });
  assert.deepEqual([...runtime.calls('websocket.send').at(-1).body], [...encode('héllo')]);
  await connection.send(new Uint8Array([0, 255]));
  assert.deepEqual(argsOf('websocket.send'), { socket: 14, text: false });
  assert.deepEqual([...runtime.calls('websocket.send').at(-1).body], [0, 255]);
  await connection.send('');
  assert.equal(typeof runtime.calls('websocket.send').at(-1).body, 'string', 'an empty message has no body but the arguments');
  assert.deepEqual(argsOf('websocket.send'), { socket: 14, text: true });

  const sends = runtime.calls('websocket.send').length;
  await connection.send(new Uint8Array(192 * 1024));
  await assert.rejects(connection.send(new Uint8Array(192 * 1024 + 1)), error => error instanceof AlefError && error.code === 'INVALID_ARGUMENT');
  await assert.rejects(connection.send('é'.repeat(96 * 1024 + 1)), error => error instanceof AlefError && error.code === 'INVALID_ARGUMENT');
  assert.equal(runtime.calls('websocket.send').length, sends + 1, 'a message too big is not sent');
});

test('close names the connection, the code and the reason, once', async () => {
  const connection = await open([endFrame()], { socket: 15 });
  const before = runtime.calls('websocket.close').length;
  await connection.close(4000, 'done');
  await connection.close();
  assert.equal(runtime.calls('websocket.close').length, before + 1);
  assert.deepEqual(argsOf('websocket.close'), { socket: 15, code: 4000, reason: 'done' });

  const plain = await open([endFrame()], { socket: 16 });
  await plain.close();
  assert.deepEqual(argsOf('websocket.close'), { socket: 16 });
});

test('a refusal of the runtime reaches the caller, and an abort reaches the call that waits', async () => {
  replies.set('websocket.connect', { status: 403, json: { code: 'PERMISSION_DENIED', message: 'no', details: { permission: 'net.http' } } });
  await assert.rejects(websocket.connect('ws://example.com/'), error => error instanceof AlefError && error.code === 'PERMISSION_DENIED' && error.details?.permission === 'net.http');
  replies.set('websocket.connect', { json: opened() });
  streams.set(71, { chunks: [endFrame()] });
  const aborted = new AbortController();
  aborted.abort();
  await assert.rejects(websocket.connect('ws://h.test/', { signal: aborted.signal }), { name: 'AbortError' });

  for (const [name, start] of [
    ['websocket.connect', signal => websocket.connect('ws://h.test/', { signal })],
    ['websocket.send', async signal => (await open([endFrame()], { socket: 17 })).send('x', { signal })],
  ]) {
    const connection = name === 'websocket.send' ? await open([endFrame()], { socket: 17 }) : null;
    hold.add(name);
    const controller = new AbortController();
    const reached = new Promise(resolve => { onHold = resolve; });
    const pending = connection ? connection.send('x', { signal: controller.signal }) : start(controller.signal);
    const outcome = pending.then(() => 'it went through', error => error?.name);
    await soon(reached);
    controller.abort();
    assert.equal(await outcome, 'AbortError', name);
    hold.delete(name);
  }
});
