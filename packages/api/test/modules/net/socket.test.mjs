// SPDX-License-Identifier: MIT OR Apache-2.0
import assert from 'node:assert/strict';
import test from 'node:test';
import { AlefError, socket, TcpServer, TcpSocket, UdpSocket } from '../../../src/index.ts';
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
const decode = bytes => new TextDecoder().decode(bytes);
const opened = (over = {}) => ({
  socket: 1, read: 11, write: 12,
  localAddress: { host: '127.0.0.1', port: 2 }, remoteAddress: { host: '10.0.0.1', port: 80 }, ...over,
});
const argsOf = command => runtime.argsOf(runtime.calls(command).at(-1));
const endless = () => ({ chunks: [endFrame()] });

async function readAll(stream) {
  const pieces = [];
  const reader = stream.getReader();
  for (;;) {
    const { done, value } = await reader.read();
    if (done) return pieces.map(decode);
    pieces.push(value);
  }
}

test('connect names the host, the port, TLS and the time, and gives the addresses and the two streams', async () => {
  replies.set('socket.connect', { json: opened() });
  streams.set(11, { chunks: [join(binaryFrame(encode('hel')), jsonFrame({ ignored: true }), binaryFrame(encode('lo')), endFrame())] });
  const conn = await socket.connect({ host: 'example.com', port: 443, tls: { serverName: 'x', ca: 'PEM' }, timeout: 900 });
  assert.ok(conn instanceof TcpSocket);
  assert.deepEqual(argsOf('socket.connect'), { host: 'example.com', port: 443, tls: { serverName: 'x', ca: 'PEM' }, timeoutMs: 900 });
  assert.deepEqual(conn.localAddress, { host: '127.0.0.1', port: 2 });
  assert.deepEqual(conn.remoteAddress, { host: '10.0.0.1', port: 80 });
  assert.deepEqual(await readAll(conn.readable), ['hel', 'lo'], 'the pieces as they came');

  streams.set(11, endless());
  await socket.connect({ host: 'example.com', port: 80, tls: true });
  assert.deepEqual(argsOf('socket.connect'), { host: 'example.com', port: 80, tls: true });
  await socket.connect({ host: 'example.com', port: 80 });
  assert.deepEqual(argsOf('socket.connect'), { host: 'example.com', port: 80 });
});

test('what is written goes to the write stream, the end of the writable ends it, an abort closes it', async () => {
  replies.set('socket.connect', { json: opened() });
  streams.set(11, endless());
  const conn = await socket.connect({ host: 'h', port: 1 });
  const writer = conn.writable.getWriter();
  const before = runtime.calls('runtime.stream.write').length;
  await writer.write(encode('abc'));
  const written = runtime.calls('runtime.stream.write').slice(before);
  assert.equal(written.length, 1);
  assert.deepEqual(runtime.argsOf(written[0]), { id: 12 });
  assert.equal(decode(written[0].body), 'abc');
  const ends = runtime.calls('runtime.stream.end').length;
  await writer.close();
  assert.equal(runtime.calls('runtime.stream.end').length, ends + 1);
  assert.deepEqual(runtime.argsOf(runtime.calls('runtime.stream.end').at(-1)), { id: 12 });

  const other = await socket.connect({ host: 'h', port: 1 });
  const closes = runtime.calls('runtime.stream.close').length;
  await other.writable.getWriter().abort();
  assert.equal(runtime.calls('runtime.stream.close').length, closes + 1);
  assert.deepEqual(runtime.argsOf(runtime.calls('runtime.stream.close').at(-1)), { id: 12 });
});

test('a socket is closed once, whatever is asked', async () => {
  replies.set('socket.connect', { json: opened({ socket: 7 }) });
  streams.set(11, endless());
  const conn = await socket.connect({ host: 'h', port: 1 });
  const before = runtime.calls('socket.close').length;
  await conn.close();
  await conn.close();
  assert.equal(runtime.calls('socket.close').length, before + 1);
  assert.deepEqual(argsOf('socket.close'), { socket: 7 });
});

test('connect needs a host and a port, and a refusal of the runtime and an abort reach the caller', async () => {
  const calls = runtime.calls('socket.connect').length;
  for (const bad of [{}, { host: 'h' }, { host: 'h', port: 1.5 }, { host: 3, port: 1 }, { port: 1 }]) {
    await assert.rejects(socket.connect(bad), error => error instanceof AlefError && error.code === 'INVALID_ARGUMENT', JSON.stringify(bad));
  }
  assert.equal(runtime.calls('socket.connect').length, calls, 'nothing was asked of the runtime');

  replies.set('socket.connect', { status: 403, json: { code: 'PERMISSION_DENIED', message: 'no', details: { permission: 'net.socket' } } });
  await assert.rejects(socket.connect({ host: 'h', port: 1 }), error => error instanceof AlefError && error.code === 'PERMISSION_DENIED' && error.details?.permission === 'net.socket');
  replies.set('socket.connect', { json: opened() });
  const controller = new AbortController();
  controller.abort();
  await assert.rejects(socket.connect({ host: 'h', port: 1, signal: controller.signal }), { name: 'AbortError' });
});

test('listen names the host and the port, and the server gives each connection as a socket', async () => {
  replies.set('socket.listen', { json: { server: 2, accept: 21, localAddress: { host: '127.0.0.1', port: 4000 } } });
  streams.set(21, {
    chunks: [join(jsonFrame(opened({ socket: 5, read: 51, write: 52 })), binaryFrame(encode('not a connection')), jsonFrame(opened({ socket: 6, read: 61, write: 62 })), endFrame())],
  });
  for (const id of [51, 61]) streams.set(id, endless());
  const server = await socket.listen({ host: '127.0.0.1', port: 0 });
  assert.ok(server instanceof TcpServer);
  assert.deepEqual(argsOf('socket.listen'), { host: '127.0.0.1', port: 0 });
  assert.deepEqual(server.localAddress, { host: '127.0.0.1', port: 4000 });
  const taken = [];
  for await (const conn of server) {
    assert.ok(conn instanceof TcpSocket);
    taken.push(conn);
  }
  assert.equal(taken.length, 2);
  await taken[0].close();
  assert.deepEqual(argsOf('socket.close'), { socket: 5 });
  await taken[1].close();
  assert.deepEqual(argsOf('socket.close'), { socket: 6 });

  await socket.listen();
  assert.deepEqual(argsOf('socket.listen'), {}, 'no host and no port: the runtime chooses');
});

test('a server that is closed ends its iteration, and a failure before that reaches the one who waits', async () => {
  replies.set('socket.listen', { json: { server: 3, accept: 22, localAddress: { host: '127.0.0.1', port: 4001 } } });
  streams.set(22, { chunks: [errorFrame({ code: 'CLOSED', message: 'closed' })] });
  const failing = await socket.listen();
  await assert.rejects((async () => { for await (const _ of failing) { /* nothing comes */ } })(), error => error instanceof AlefError && error.code === 'CLOSED');

  const closed = await socket.listen();
  const before = runtime.calls('socket.close').length;
  await closed.close();
  await closed.close();
  assert.equal(runtime.calls('socket.close').length, before + 1);
  assert.deepEqual(argsOf('socket.close'), { socket: 3 });
  for await (const _ of closed) assert.fail('a closed server gives nothing');
});

test('udp names the place, sends a datagram of a string or of bytes, and gives the datagrams that arrive', async () => {
  replies.set('socket.udp', { json: { socket: 8, messages: 31, localAddress: { host: '127.0.0.1', port: 5000 } } });
  streams.set(31, {
    chunks: [join(
      jsonFrame({ host: '10.0.0.2', port: 53, data: btoa('pong') }),
      jsonFrame({ host: '10.0.0.3', port: 54, data: '' }),
      binaryFrame(encode('ignored')),
      endFrame(),
    )],
  });
  const udp = await socket.udp({ host: '127.0.0.1', port: 5000 });
  assert.ok(udp instanceof UdpSocket);
  assert.deepEqual(argsOf('socket.udp'), { host: '127.0.0.1', port: 5000 });
  assert.deepEqual(udp.localAddress, { host: '127.0.0.1', port: 5000 });

  const got = [];
  for await (const datagram of udp) got.push({ ...datagram, data: decode(datagram.data) });
  assert.deepEqual(got, [{ host: '10.0.0.2', port: 53, data: 'pong' }, { host: '10.0.0.3', port: 54, data: '' }]);

  await udp.send('héllo', 'dns.test', 53);
  assert.deepEqual(argsOf('socket.send'), { socket: 8, host: 'dns.test', port: 53 });
  assert.deepEqual([...runtime.calls('socket.send').at(-1).body], [...encode('héllo')], 'a string is its UTF-8');
  await udp.send(new Uint8Array([0, 255, 7]), 'dns.test', 54);
  assert.deepEqual([...runtime.calls('socket.send').at(-1).body], [0, 255, 7]);
  await udp.send(new Uint8Array(0), 'dns.test', 55);
  assert.equal(typeof runtime.calls('socket.send').at(-1).body, 'string', 'an empty datagram has no body but the arguments');
  const controller = new AbortController();
  controller.abort();
  await assert.rejects(udp.send('x', 'dns.test', 53, { signal: controller.signal }), { name: 'AbortError' });

  await socket.udp();
  assert.deepEqual(argsOf('socket.udp'), {});
});

test('a udp socket is closed once, and a failure of its stream is for the one who waits unless it was closed', async () => {
  replies.set('socket.udp', { json: { socket: 9, messages: 32, localAddress: { host: '127.0.0.1', port: 5001 } } });
  streams.set(32, { chunks: [errorFrame({ code: 'CLOSED', message: 'closed' })] });
  const udp = await socket.udp();
  await assert.rejects((async () => { for await (const _ of udp) { /* nothing comes */ } })(), error => error instanceof AlefError && error.code === 'CLOSED');
  const before = runtime.calls('socket.close').length;
  await udp.close();
  await udp.close();
  assert.equal(runtime.calls('socket.close').length, before + 1);
  assert.deepEqual(argsOf('socket.close'), { socket: 9 });
  for await (const _ of udp) assert.fail('a closed socket gives nothing');
});

test('leaving the iteration early closes the stream of connections and the stream of datagrams', async () => {
  replies.set('socket.listen', { json: { server: 4, accept: 23, localAddress: { host: '127.0.0.1', port: 4002 } } });
  streams.set(23, { chunks: [join(jsonFrame(opened({ socket: 8, read: 81, write: 82 })), jsonFrame(opened({ socket: 9, read: 91, write: 92 })), endFrame())] });
  for (const id of [81, 91]) streams.set(id, endless());
  const server = await socket.listen();
  let closes = runtime.calls('runtime.stream.close').length;
  for await (const conn of server) {
    assert.ok(conn instanceof TcpSocket);
    break;
  }
  assert.deepEqual(argsOf('runtime.stream.close'), { id: 23 });
  assert.equal(runtime.calls('runtime.stream.close').length, closes + 1);

  replies.set('socket.udp', { json: { socket: 10, messages: 33, localAddress: { host: '127.0.0.1', port: 5002 } } });
  streams.set(33, { chunks: [join(jsonFrame({ host: 'h', port: 1, data: '' }), jsonFrame({ host: 'h', port: 2, data: '' }), endFrame())] });
  const udp = await socket.udp();
  closes = runtime.calls('runtime.stream.close').length;
  for await (const _ of udp) break;
  assert.deepEqual(argsOf('runtime.stream.close'), { id: 33 });
  assert.equal(runtime.calls('runtime.stream.close').length, closes + 1);
});

test('an abort reaches the call that waits, for connect, listen and udp', async () => {
  replies.set('socket.connect', { json: opened() });
  replies.set('socket.listen', { json: { server: 2, accept: 21, localAddress: { host: '127.0.0.1', port: 4000 } } });
  replies.set('socket.udp', { json: { socket: 3, messages: 31, localAddress: { host: '127.0.0.1', port: 5000 } } });
  for (const id of [11, 21, 31]) streams.set(id, endless());
  const calls = {
    'socket.connect': signal => socket.connect({ host: 'h', port: 1, signal }),
    'socket.listen': signal => socket.listen({ signal }),
    'socket.udp': signal => socket.udp({ signal }),
  };
  for (const [name, start] of Object.entries(calls)) {
    hold.add(name);
    const controller = new AbortController();
    const reached = new Promise(resolve => { onHold = resolve; });
    const outcome = start(controller.signal).then(() => 'it went through', error => error?.name);
    await soon(reached);
    controller.abort();
    assert.equal(await outcome, 'AbortError', name);
    hold.delete(name);
  }
});
