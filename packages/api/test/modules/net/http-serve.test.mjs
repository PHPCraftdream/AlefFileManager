// SPDX-License-Identifier: MIT OR Apache-2.0
import assert from 'node:assert/strict';
import test from 'node:test';
import { AlefError, http, HttpServer, ServerRequest } from '../../../src/index.ts';
import { binaryFrame, endFrame, errorFrame, installRuntime, join, jsonFrame } from '../../fake-runtime.mjs';

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
const frameOf = (over = {}) => ({ id: 70, method: 'GET', url: '/a?b=1', headers: [['host', '127.0.0.1:80'], ['x-two', 'a'], ['x-two', 'b']], body: null, ...over });
const opened = (over = {}) => ({ server: 3, requests: 51, address: { host: '127.0.0.1', port: 8080 }, secure: false, ...over });

async function serving(requests = [], over = {}) {
  replies.set('http.serve', { json: opened(over) });
  streams.set(51, { chunks: [join(...requests.map(jsonFrame), endFrame())] });
  return http.serve();
}

async function first(server) {
  for await (const request of server) return request;
  throw new Error('no request came');
}

test('serve names the address, the port, TLS, the folder, the names, the origins and the time', async () => {
  await serving();
  assert.deepEqual(argsOf('http.serve'), {}, 'nothing asked: the runtime chooses');
  await http.serve({
    host: '0.0.0.0', port: 8443, tls: { cert: 'C', key: 'K' }, files: '/site', hosts: ['app.test'], origins: ['https://app.test'], answerTimeout: 900,
  });
  assert.deepEqual(argsOf('http.serve'), {
    host: '0.0.0.0', port: 8443, tls: { cert: 'C', key: 'K' }, files: '/site', hosts: ['app.test'], origins: ['https://app.test'], answerTimeoutMs: 900,
  });
});

test('the server tells its address and where clients come, with brackets for an address of IPv6', async () => {
  const plain = await serving();
  assert.ok(plain instanceof HttpServer);
  assert.deepEqual(plain.address, { host: '127.0.0.1', port: 8080 });
  assert.equal(plain.secure, false);
  assert.equal(plain.url, 'http://127.0.0.1:8080');
  const secure = await serving([], { secure: true, address: { host: '::1', port: 9 } });
  assert.equal(secure.secure, true);
  assert.equal(secure.url, 'https://[::1]:9');
});

test('the iteration gives the requests as they come, with their head and no body when there is none', async () => {
  const server = await serving([frameOf(), frameOf({ id: 71, method: 'DELETE', url: '/' })]);
  const seen = [];
  for await (const request of server) {
    assert.ok(request instanceof ServerRequest);
    seen.push(request);
  }
  assert.deepEqual(seen.map(request => [request.method, request.url]), [['GET', '/a?b=1'], ['DELETE', '/']]);
  assert.equal(seen[0].headers.get('x-two'), 'a, b', 'the headers that repeat are joined');
  assert.equal(seen[0].headers.get('host'), '127.0.0.1:80');
  assert.equal(seen[0].body, null);
  assert.equal(seen[0].bodyUsed, false);
  assert.deepEqual([...await seen[0].bytes()], [], 'no body reads as empty');
});

test('the body of a request is read once as a stream, bytes, text or json', async () => {
  streams.set(61, { chunks: [join(binaryFrame(encode('{"n":')), binaryFrame(encode('42}')), endFrame())] });
  const request = await first(await serving([frameOf({ body: 61 })]));
  assert.deepEqual(await request.json(), { n: 42 });
  assert.equal(request.bodyUsed, true);
  assert.equal(request.body, null, 'a body is taken once');
  await assert.rejects(request.text(), error => error instanceof AlefError && error.code === 'INVALID_ARGUMENT');

  streams.set(62, { chunks: [join(binaryFrame(encode('héllo')), endFrame())] });
  assert.equal(await (await first(await serving([frameOf({ body: 62 })]))).text(), 'héllo');

  streams.set(63, { chunks: [join(binaryFrame(new Uint8Array([1, 2])), jsonFrame({ ignored: true }), binaryFrame(new Uint8Array([3])), endFrame())] });
  const pieces = [];
  const reader = (await first(await serving([frameOf({ body: 63 })]))).body.getReader();
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    pieces.push([...value]);
  }
  assert.deepEqual(pieces, [[1, 2], [3]], 'the pieces as they came');

  streams.set(64, { chunks: [join(binaryFrame(encode('half')), errorFrame({ code: 'NETWORK', message: 'broke' }))] });
  const broken = await first(await serving([frameOf({ body: 64 })]));
  await assert.rejects(broken.bytes(), error => error instanceof AlefError && error.code === 'NETWORK');
});

test('an answer names the request, the status and the headers; with no body it travels alone', async () => {
  const request = await first(await serving([frameOf({ id: 72 })]));
  await request.respond();
  assert.deepEqual(argsOf('http.respond'), { request: 72, headers: [] });
  assert.equal(runtime.calls('http.respond').at(-1).body, '{"request":72,"headers":[]}');

  const second = await first(await serving([frameOf({ id: 73 })]));
  await second.respond({ status: 204, headers: { 'x-a': '1' } });
  assert.deepEqual(argsOf('http.respond'), { request: 73, status: 204, headers: [['x-a', '1']] });

  const third = await first(await serving([frameOf({ id: 74 })]));
  await third.respond({ status: 404 });
  assert.deepEqual(argsOf('http.respond'), { request: 74, status: 404, headers: [] });
});

test('a text and bytes that fit in a call go with the call as their bytes', async () => {
  const request = await first(await serving([frameOf({ id: 75 })]));
  await request.respond({ body: 'héllo', status: 201, headers: [['content-type', 'text/plain']] });
  const sent = runtime.calls('http.respond').at(-1);
  assert.deepEqual(argsOf('http.respond'), { request: 75, status: 201, headers: [['content-type', 'text/plain']] });
  assert.deepEqual([...sent.body], [...encode('héllo')]);

  const second = await first(await serving([frameOf({ id: 76 })]));
  await second.respond({ body: new Uint8Array([7, 8, 9]) });
  assert.deepEqual([...runtime.calls('http.respond').at(-1).body], [7, 8, 9]);

  const empty = await first(await serving([frameOf({ id: 77 })]));
  const before = runtime.calls('http.respondStream').length;
  await empty.respond({ body: '' });
  assert.equal(runtime.calls('http.respond').at(-1).body, '{"request":77,"headers":[]}', 'an empty text has no body');
  assert.equal(runtime.calls('http.respondStream').length, before);
});

test('a body of exactly the size of a call goes with the call, and one byte more goes as a stream', async () => {
  replies.set('http.respondStream', { json: { upload: 81 } });
  const fits = await first(await serving([frameOf({ id: 78 })]));
  const streamed = runtime.calls('http.respondStream').length;
  await fits.respond({ body: new Uint8Array(192 * 1024) });
  assert.equal(runtime.calls('http.respondStream').length, streamed);

  const over = await first(await serving([frameOf({ id: 79 })]));
  const before = runtime.calls('runtime.stream.write').length;
  await over.respond({ body: new Uint8Array(192 * 1024 + 1).fill(5), status: 200 });
  assert.equal(runtime.calls('http.respondStream').length, streamed + 1);
  assert.deepEqual(argsOf('http.respondStream'), { request: 79, status: 200, headers: [] });
  const written = runtime.calls('runtime.stream.write').slice(before).reduce((sum, call) => sum + call.body.length, 0);
  assert.equal(written, 192 * 1024 + 1, 'all of it went down');
  assert.ok(runtime.calls('runtime.stream.end').length >= 1, 'and the stream was ended');
});

test('a stream and a big text go down through a stream, as they are read', async () => {
  replies.set('http.respondStream', { json: { upload: 82 } });
  const request = await first(await serving([frameOf({ id: 80 })]));
  const source = new ReadableStream({
    start(controller) {
      controller.enqueue(new Uint8Array([1, 2]));
      controller.enqueue(new Uint8Array([3]));
      controller.close();
    },
  });
  const before = runtime.calls('runtime.stream.write').length;
  await request.respond({ body: source, headers: { 'content-type': 'application/octet-stream' } });
  assert.deepEqual(argsOf('http.respondStream'), { request: 80, headers: [['content-type', 'application/octet-stream']] });
  assert.deepEqual(runtime.calls('runtime.stream.write').slice(before).map(call => [...call.body]), [[1, 2], [3]]);

  const text = await first(await serving([frameOf({ id: 83 })]));
  const mark = runtime.calls('runtime.stream.write').length;
  await text.respond({ body: 'é'.repeat(200 * 1024) });
  const written = runtime.calls('runtime.stream.write').slice(mark).reduce((sum, call) => sum + call.body.length, 0);
  assert.equal(written, 400 * 1024, 'a big text goes as its UTF-8 bytes');
});

test('a stream that fails while it goes down closes the stream and is no answer as if it were whole', async () => {
  replies.set('http.respondStream', { json: { upload: 84 } });
  const request = await first(await serving([frameOf({ id: 85 })]));
  let pulls = 0;
  const broken = new ReadableStream({
    pull(controller) {
      pulls += 1;
      if (pulls === 1) controller.enqueue(new Uint8Array([1]));
      else controller.error(new Error('the source broke'));
    },
  });
  const closes = runtime.calls('runtime.stream.close').length;
  const ends = runtime.calls('runtime.stream.end').length;
  await assert.rejects(request.respond({ body: broken }), /the source broke/);
  assert.equal(runtime.calls('runtime.stream.close').length, closes + 1);
  assert.equal(runtime.calls('runtime.stream.end').length, ends);
  await assert.rejects(request.respond({ body: 'again' }), error => error instanceof AlefError && error.code === 'INVALID_ARGUMENT', 'the runtime took the request when the stream began');
});

test('an aborted signal stops serve, and a frame of another kind is no request', async () => {
  const controller = new AbortController();
  controller.abort();
  await assert.rejects(http.serve({ signal: controller.signal }), { name: 'AbortError' });
  replies.set('http.serve', { json: opened() });
  streams.set(51, { chunks: [join(binaryFrame(encode('noise')), jsonFrame(frameOf({ id: 90 })), binaryFrame(encode('more')), endFrame())] });
  const seen = [];
  for await (const request of await http.serve()) seen.push(request.url);
  assert.deepEqual(seen, ['/a?b=1'], 'only the frames of JSON are requests');
});

test('a request is answered once, but an answer the runtime refused may be given again', async () => {
  const request = await first(await serving([frameOf({ id: 86 })]));
  replies.set('http.respond', { status: 400, json: { code: 'INVALID_ARGUMENT', message: 'a status is 200 to 599' } });
  await assert.rejects(request.respond({ body: 'x', status: 99 }), error => error instanceof AlefError && error.code === 'INVALID_ARGUMENT');
  replies.set('http.respond', { json: null });
  const calls = runtime.calls('http.respond').length;
  await request.respond({ body: 'x', status: 200 });
  assert.equal(runtime.calls('http.respond').length, calls + 1);
  await assert.rejects(request.respond({ body: 'again' }), error => error instanceof AlefError && error.code === 'INVALID_ARGUMENT');
  assert.equal(runtime.calls('http.respond').length, calls + 1, 'the second answer did not reach the runtime');
});

test('close stops the server once, and the iteration that was waiting ends without an error', async () => {
  const server = await serving();
  streams.set(51, { status: 500, json: { code: 'INTERNAL', message: 'the stream went with the server' } });
  const before = runtime.calls('socket.close').length;
  await server.close();
  await server.close();
  assert.equal(runtime.calls('socket.close').length, before + 1);
  assert.deepEqual(argsOf('socket.close'), { socket: 3 });
  const seen = [];
  for await (const request of server) seen.push(request);
  assert.deepEqual(seen, [], 'a server that was closed ends its iteration quietly');
});

test('a stream of requests that fails while the server is open fails the iteration', async () => {
  const server = await serving();
  streams.set(51, { chunks: [join(jsonFrame(frameOf()), errorFrame({ code: 'INTERNAL', message: 'lost' }))] });
  await assert.rejects(
    (async () => {
      for await (const request of server) assert.equal(request.method, 'GET');
    })(),
    error => error instanceof AlefError && error.code === 'INTERNAL',
  );
});
