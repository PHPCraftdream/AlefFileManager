// SPDX-License-Identifier: MIT OR Apache-2.0
import assert from 'node:assert/strict';
import test from 'node:test';
import { AlefError, http } from '../../../src/index.ts';
import { binaryFrame, endFrame, installRuntime, join, jsonFrame } from '../../fake-runtime.mjs';

const replies = new Map();
const streams = new Map();
const runtime = installRuntime({
  limits: { chunkSize: 64 * 1024 },
  handler(request) {
    const stream = /^native:\/\/stream\/(\d+)$/.exec(request.url);
    if (stream) return streams.get(Number(stream[1])) ?? { status: 404, json: { code: 'NOT_FOUND', message: 'stream not found' } };
    return replies.get(request.url.replace('native://call/', '')) ?? { json: null };
  },
});

const encode = text => new TextEncoder().encode(text);
const argsOf = command => runtime.argsOf(runtime.calls(command).at(-1));
const frameOf = (over = {}) => ({ id: 70, method: 'GET', url: '/a?b=1', headers: [], body: null, ...over });
const offer = (over = {}) => frameOf({ id: 95, upgrade: true, protocols: ['chat', 'superchat'], ...over });

async function serving(requests) {
  replies.set('http.serve', { json: { server: 3, requests: 51, address: { host: '127.0.0.1', port: 8080 }, secure: false } });
  streams.set(51, { chunks: [join(...requests.map(jsonFrame), endFrame())] });
  return http.serve();
}

async function first(server) {
  for await (const request of server) return request;
  throw new Error('no request came');
}

const invalid = error => error instanceof AlefError && error.code === 'INVALID_ARGUMENT';

test('a request tells whether it offers a WebSocket and which subprotocols', async () => {
  const seen = [];
  for await (const request of await serving([frameOf(), offer()])) seen.push(request);
  const [plain, offered] = seen;
  assert.equal(plain.upgradable, false);
  assert.deepEqual([...plain.protocols], []);
  assert.equal(offered.upgradable, true);
  assert.deepEqual([...offered.protocols], ['chat', 'superchat']);
});

test('upgrade takes the offer and gives a connection that sends, reads and is named by the request', async () => {
  replies.set('http.upgrade', { json: { socket: 8, messages: 81, protocol: 'superchat' } });
  streams.set(81, { chunks: [join(jsonFrame({ type: 'text', length: 2 }), binaryFrame(encode('hi')), endFrame())] });
  const request = await first(await serving([offer({ url: '/chat?room=1' })]));
  const connection = await request.upgrade({ protocol: 'superchat' });
  assert.deepEqual(argsOf('http.upgrade'), { request: 95, protocol: 'superchat' });
  assert.equal(connection.protocol, 'superchat');
  assert.equal(connection.url, '/chat?room=1', 'the address is the one the client asked for');
  const heard = [];
  for await (const message of connection) heard.push(message);
  assert.deepEqual(heard, [{ type: 'text', data: 'hi' }]);
  await connection.send('back');
  assert.deepEqual(argsOf('websocket.send'), { socket: 8, text: true });
  await assert.rejects(request.respond({ body: 'x' }), invalid, 'an offer taken is answered');
  await assert.rejects(request.upgrade(), invalid);
});

test('upgrade names no subprotocol when none is given', async () => {
  replies.set('http.upgrade', { json: { socket: 9, messages: 91, protocol: '' } });
  streams.set(91, { chunks: [endFrame()] });
  const request = await first(await serving([offer({ id: 96 })]));
  const connection = await request.upgrade();
  assert.deepEqual(argsOf('http.upgrade'), { request: 96 });
  assert.equal(connection.protocol, '');
});

test('a request that offers nothing cannot be upgraded, and the runtime is not asked', async () => {
  const request = await first(await serving([frameOf({ id: 97 })]));
  const calls = runtime.calls('http.upgrade').length;
  await assert.rejects(request.upgrade(), invalid);
  assert.equal(runtime.calls('http.upgrade').length, calls);
  replies.set('http.respond', { json: null });
  await request.respond({ status: 426 });
  assert.deepEqual(argsOf('http.respond'), { request: 97, status: 426, headers: [] }, 'it is answered otherwise');
});

test('an upgrade the runtime refused may be tried again, and one after an answer may not', async () => {
  const request = await first(await serving([offer({ id: 98 })]));
  replies.set('http.upgrade', { status: 400, json: { code: 'INVALID_ARGUMENT', message: 'the subprotocol was not offered' } });
  await assert.rejects(request.upgrade({ protocol: 'other' }), invalid);
  replies.set('http.upgrade', { json: { socket: 10, messages: 101, protocol: 'chat' } });
  streams.set(101, { chunks: [endFrame()] });
  const connection = await request.upgrade({ protocol: 'chat' });
  assert.equal(connection.protocol, 'chat');

  replies.set('http.respond', { json: null });
  const answered = await first(await serving([offer({ id: 99 })]));
  await answered.respond({ status: 403 });
  const calls = runtime.calls('http.upgrade').length;
  await assert.rejects(answered.upgrade(), invalid);
  assert.equal(runtime.calls('http.upgrade').length, calls, 'the runtime was not asked');
});
