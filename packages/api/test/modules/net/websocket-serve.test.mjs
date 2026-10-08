// SPDX-License-Identifier: MIT OR Apache-2.0
import assert from 'node:assert/strict';
import test from 'node:test';
import { AlefError, websocket, WebSocketConnection } from '../../../src/index.ts';
import { endFrame, installRuntime, join, jsonFrame } from '../../fake-runtime.mjs';

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

const argsOf = command => runtime.argsOf(runtime.calls(command).at(-1));
const offer = (id, over = {}) => ({ id, method: 'GET', url: '/chat', headers: [], body: null, upgrade: true, protocols: [], ...over });
const plain = (id, over = {}) => ({ id, method: 'GET', url: '/chat', headers: [], body: null, upgrade: false, protocols: [], ...over });
const answers = () => runtime.calls('http.respond').map(call => runtime.argsOf(call));
const saidTo = id => new TextDecoder().decode(runtime.calls('http.respond').find(call => runtime.argsOf(call).request === id).body);
const upgrades = () => runtime.calls('http.upgrade').map(call => runtime.argsOf(call));

/** A server of WebSocket whose clients are the given requests; every upgrade gives a connection. */
async function serving(requests, options = {}, opened = {}) {
  replies.set('http.serve', { json: { server: 3, requests: 51, address: { host: '127.0.0.1', port: 8080 }, secure: false, ...opened } });
  streams.set(51, { chunks: [join(...requests.map(jsonFrame), endFrame())] });
  replies.set('http.upgrade', { json: { socket: 20, messages: 90, protocol: '' } });
  replies.set('http.respond', { json: null });
  streams.set(90, { chunks: [endFrame()] });
  return websocket.serve(options);
}

async function all(server) {
  const seen = [];
  for await (const connection of server) seen.push(connection);
  return seen;
}

test('serve gives the listening options to the server of HTTP and keeps the path and the subprotocols for itself', async () => {
  await serving([], {
    host: '127.0.0.1', port: 9000, tls: { cert: 'C', key: 'K' }, hosts: ['app.test'], origins: ['https://app.test'], path: '/chat', protocols: ['chat'],
  });
  assert.deepEqual(argsOf('http.serve'), {
    host: '127.0.0.1', port: 9000, tls: { cert: 'C', key: 'K' }, hosts: ['app.test'], origins: ['https://app.test'],
  });
  await serving([]);
  assert.deepEqual(argsOf('http.serve'), {});
});

test('the server tells its address and where clients come, with the path', async () => {
  const server = await serving([], { path: '/chat' });
  assert.deepEqual(server.address, { host: '127.0.0.1', port: 8080 });
  assert.equal(server.secure, false);
  assert.equal(server.url, 'ws://127.0.0.1:8080/chat');
  const secure = await serving([], {}, { secure: true, address: { host: '::1', port: 9 } });
  assert.equal(secure.secure, true);
  assert.equal(secure.url, 'wss://[::1]:9');
});

test('a path starts with a slash, and the runtime is not asked when it does not', async () => {
  const calls = runtime.calls('http.serve').length;
  await assert.rejects(websocket.serve({ path: 'chat' }), error => error instanceof AlefError && error.code === 'INVALID_ARGUMENT');
  assert.equal(runtime.calls('http.serve').length, calls);
});

test('the iteration gives an open connection for each offer, in the order they came', async () => {
  const server = await serving([offer(1), offer(2)]);
  const before = upgrades().length;
  const connections = await all(server);
  assert.equal(connections.length, 2);
  assert.ok(connections.every(connection => connection instanceof WebSocketConnection));
  assert.deepEqual(upgrades().slice(before), [{ request: 1 }, { request: 2 }]);
});

test('the subprotocol is the first of the server that the client offered', async () => {
  const server = await serving(
    [offer(3, { protocols: ['superchat', 'chat'] }), offer(4, { protocols: ['x'] }), offer(5, { protocols: [] }), offer(6, { protocols: ['chat'] })],
    { protocols: ['chat', 'superchat'] },
  );
  const before = upgrades().length;
  const connections = await all(server);
  assert.equal(connections.length, 3, 'the client that offers some and none in common is refused');
  assert.deepEqual(upgrades().slice(before), [{ request: 3, protocol: 'chat' }, { request: 5 }, { request: 6, protocol: 'chat' }]);
  assert.equal(answers().find(args => args.request === 4).status, 400);
  assert.equal(saidTo(4), 'No subprotocol in common.');
});

test('the subprotocols the client offers are no business of a server that names none', async () => {
  const server = await serving([offer(7, { protocols: ['chat'] })]);
  await all(server);
  assert.deepEqual(argsOf('http.upgrade'), { request: 7 });
});

test('what is no offer is answered with a 426, and a path that is not the servers with a 404', async () => {
  const server = await serving([plain(8), offer(9, { url: '/other?x=1' }), offer(10, { url: '/chat?x=1' })], { path: '/chat' });
  const connections = await all(server);
  assert.equal(connections.length, 1);
  assert.deepEqual(answers().find(args => args.request === 8), { request: 8, status: 426, headers: [['sec-websocket-version', '13']] });
  assert.equal(answers().find(args => args.request === 9).status, 404);
  assert.equal(saidTo(8), 'This server speaks WebSocket.');
  assert.equal(saidTo(9), 'No WebSocket here.');
  assert.equal(upgrades().some(args => args.request === 9), false, 'the other path was not upgraded');
});

test('a request that cannot be upgraded or answered is left, and the iteration goes on', async () => {
  const server = await serving([offer(11), offer(12), plain(13)]);
  replies.set('http.upgrade', { status: 404, json: { code: 'NOT_FOUND', message: 'the client went away' } });
  replies.set('http.respond', { status: 404, json: { code: 'NOT_FOUND', message: 'the client went away' } });
  const before = upgrades().length;
  const connections = await all(server);
  assert.deepEqual(connections, [], 'no connection, and no error');
  assert.equal(upgrades().length - before, 2, 'both offers were tried');
});

test('close stops the server of HTTP once', async () => {
  const server = await serving([]);
  const before = runtime.calls('socket.close').length;
  await server.close();
  await server.close();
  assert.equal(runtime.calls('socket.close').length, before + 1);
  assert.deepEqual(argsOf('socket.close'), { socket: 3 });
});
