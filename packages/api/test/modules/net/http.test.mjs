// SPDX-License-Identifier: MIT OR Apache-2.0
import assert from 'node:assert/strict';
import test from 'node:test';
import { AlefError, http, HttpResponse } from '../../../src/index.ts';
import { binaryFrame, endFrame, errorFrame, installRuntime, join, jsonFrame, liveStream } from '../../fake-runtime.mjs';

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

const head = (over = {}) => ({
  status: 200, statusText: 'OK', url: 'http://127.0.0.1/a', redirected: false,
  headers: [['content-type', 'text/plain'], ['x-two', 'a'], ['x-two', 'b']], stream: null, ...over,
});
const encode = text => new TextEncoder().encode(text);

async function sent(command, fn, reply) {
  replies.set(command, reply);
  const before = runtime.calls(command).length;
  const result = await fn();
  const calls = runtime.calls(command);
  assert.equal(calls.length, before + 1, `${command} was called once`);
  const request = calls.at(-1);
  return { result, args: runtime.argsOf(request), body: request.body };
}

test('a request names the address, method, headers, timeout and redirect, and a small body travels with the call', async () => {
  const plain = await sent('http.request', () => http.request('http://127.0.0.1/a'), { json: head() });
  assert.deepEqual(plain.args, { url: 'http://127.0.0.1/a', headers: [] });
  assert.equal(plain.body, '{"url":"http://127.0.0.1/a","headers":[]}', 'no body: the arguments are the JSON');

  const full = await sent(
    'http.request',
    () => http.request('http://127.0.0.1/a', { method: 'POST', headers: { 'X-One': '1', accept: 'text/plain' }, body: 'héllo', timeout: 1500, redirect: 'manual' }),
    { json: head() },
  );
  assert.deepEqual(full.args, {
    url: 'http://127.0.0.1/a', method: 'POST', headers: [['accept', 'text/plain'], ['x-one', '1']], timeoutMs: 1500, redirect: 'manual',
  });
  assert.deepEqual([...full.body], [...encode('héllo')], 'a string is its UTF-8');

  const raw = await sent('http.request', () => http.request('http://127.0.0.1/a', { method: 'PUT', body: new Uint8Array([0, 255, 7]) }), { json: head() });
  assert.deepEqual([...raw.body], [0, 255, 7]);
  const empty = await sent('http.request', () => http.request('http://127.0.0.1/a', { method: 'POST', body: new Uint8Array(0) }), { json: head() });
  assert.equal(typeof empty.body, 'string', 'an empty body is no body');
});

test('the answer has its head, and its body is read once as a stream, bytes, text or json', async () => {
  const answer = (await sent('http.request', () => http.request('http://127.0.0.1/a'), { json: head({ stream: 11 }) })).result;
  assert.ok(answer instanceof HttpResponse);
  assert.deepEqual([answer.status, answer.statusText, answer.url, answer.redirected, answer.ok], [200, 'OK', 'http://127.0.0.1/a', false, true]);
  assert.equal(answer.headers.get('content-type'), 'text/plain');
  assert.equal(answer.headers.get('x-two'), 'a, b');
  streams.set(11, { chunks: [join(binaryFrame(encode('{"n":')), binaryFrame(encode('42}')), endFrame())] });
  assert.deepEqual(await answer.json(), { n: 42 });
  assert.equal(answer.bodyUsed, true);
  await assert.rejects(answer.text(), error => error instanceof AlefError && error.code === 'INVALID_ARGUMENT', 'a body is read once');

  const text = (await sent('http.request', () => http.request('http://127.0.0.1/a'), { json: head({ stream: 12 }) })).result;
  streams.set(12, { chunks: [join(binaryFrame(encode('héllo')), endFrame())] });
  assert.equal(await text.text(), 'héllo');

  const piecewise = (await sent('http.request', () => http.request('http://127.0.0.1/a'), { json: head({ stream: 13 }) })).result;
  streams.set(13, { chunks: [join(binaryFrame(new Uint8Array([1, 2])), jsonFrame({ ignored: true }), binaryFrame(new Uint8Array([3])), endFrame())] });
  const pieces = [];
  const reader = piecewise.body.getReader();
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    pieces.push([...value]);
  }
  assert.deepEqual(pieces, [[1, 2], [3]], 'the stream gives the pieces as they came');

  const none = (await sent('http.request', () => http.request('http://127.0.0.1/a'), { json: head({ status: 404, statusText: 'Not Found' }) })).result;
  assert.equal(none.ok, false);
  assert.equal(none.body, null, 'no stream, no body');
  assert.deepEqual([...await none.bytes()], []);

  const broken = (await sent('http.request', () => http.request('http://127.0.0.1/a'), { json: head({ stream: 14 }) })).result;
  streams.set(14, { chunks: [join(binaryFrame(encode('half')), errorFrame({ code: 'NETWORK', message: 'broke' }))] });
  await assert.rejects(broken.bytes(), error => error instanceof AlefError && error.code === 'NETWORK');
});

test('a big body and a stream go up through a stream, and the answer is asked for after', async () => {
  replies.set('http.start', { json: { request: 5, upload: 31 } });
  replies.set('http.response', { json: head({ status: 201, statusText: 'Created' }) });
  const big = new Uint8Array(300 * 1024).fill(9);
  const before = runtime.calls('runtime.stream.write').length;
  const answer = await http.request('http://127.0.0.1/up', { method: 'POST', body: big, headers: { 'content-type': 'application/octet-stream' } });
  assert.equal(answer.status, 201);
  assert.deepEqual(runtime.argsOf(runtime.calls('http.start').at(-1)), {
    url: 'http://127.0.0.1/up', method: 'POST', headers: [['content-type', 'application/octet-stream']],
  });
  const written = runtime.calls('runtime.stream.write').slice(before).reduce((sum, call) => sum + call.body.length, 0);
  assert.equal(written, big.length, 'all of it went up');
  assert.ok(runtime.calls('runtime.stream.end').length >= 1, 'and the stream was ended');
  assert.deepEqual(runtime.argsOf(runtime.calls('http.response').at(-1)), { request: 5 });

  const fromStream = new ReadableStream({
    start(controller) {
      controller.enqueue(new Uint8Array([1, 2]));
      controller.enqueue(new Uint8Array([3]));
      controller.close();
    },
  });
  const mark = runtime.calls('runtime.stream.write').length;
  await http.request('http://127.0.0.1/up', { method: 'POST', body: fromStream });
  assert.deepEqual(runtime.calls('runtime.stream.write').slice(mark).map(call => [...call.body]), [[1, 2], [3]]);
  const small = runtime.calls('http.start').length;
  await http.request('http://127.0.0.1/up', { method: 'POST', body: new Uint8Array(10) });
  assert.equal(runtime.calls('http.start').length, small, 'a small body does not need a stream');
});

test('download names the address and the file, reports the progress and ends with the stream', async () => {
  replies.set('http.download', { json: { stream: 41 } });
  streams.set(41, {
    chunks: [join(jsonFrame({ received: 10, total: 30 }), binaryFrame(new Uint8Array([9])), jsonFrame({ received: 30, total: 30, done: true }), endFrame())],
  });
  const progress = [];
  await http.download('http://127.0.0.1/f', '/tmp/f', { headers: { accept: '*/*' }, timeout: 900, onProgress: item => progress.push(item) });
  assert.deepEqual(runtime.argsOf(runtime.calls('http.download').at(-1)), {
    url: 'http://127.0.0.1/f', path: '/tmp/f', headers: [['accept', '*/*']], timeoutMs: 900,
  });
  assert.deepEqual(progress, [{ received: 10, total: 30 }, { received: 30, total: 30 }]);

  replies.set('http.download', { json: { stream: 42 } });
  streams.set(42, { chunks: [join(jsonFrame({ received: 5, total: null }), errorFrame({ code: 'NETWORK', message: 'broke' }))] });
  await assert.rejects(http.download('http://127.0.0.1/f', '/tmp/f'), error => error instanceof AlefError && error.code === 'NETWORK');
});

test('a refusal of the runtime reaches the caller, and an aborted request fails with the abort', async () => {
  replies.set('http.request', { status: 403, json: { code: 'PERMISSION_DENIED', message: 'no', details: { permission: 'net.http' } } });
  await assert.rejects(http.request('http://example.com/'), error => error instanceof AlefError
    && error.code === 'PERMISSION_DENIED' && error.details?.permission === 'net.http');
  replies.set('http.request', { json: head() });
  const controller = new AbortController();
  controller.abort();
  await assert.rejects(http.request('http://127.0.0.1/a', { signal: controller.signal }), { name: 'AbortError' });
  await assert.rejects(http.download('http://127.0.0.1/a', '/tmp/x', { signal: controller.signal }), { name: 'AbortError' });
});

test('an answer is ok from 200 to 299 and says whether it came after a redirect', async () => {
  for (const [status, ok] of [[199, false], [200, true], [299, true], [300, false], [404, false]]) {
    const answer = (await sent('http.request', () => http.request('http://127.0.0.1/a'), { json: head({ status }) })).result;
    assert.equal(answer.ok, ok, String(status));
  }
  const moved = (await sent('http.request', () => http.request('http://127.0.0.1/a'), { json: head({ redirected: true }) })).result;
  assert.equal(moved.redirected, true);
});

test('an abort reaches the call that waits, in each step of a request', async () => {
  replies.set('http.start', { json: { request: 10, upload: 36 } });
  replies.set('http.response', { json: head() });
  replies.set('http.request', { json: head() });
  const big = new Uint8Array(300 * 1024);
  for (const [name, body] of [['http.request', undefined], ['http.start', big], ['http.response', big]]) {
    hold.add(name);
    const controller = new AbortController();
    const reached = new Promise(resolve => { onHold = resolve; });
    const pending = http.request('http://127.0.0.1/a', { method: 'POST', body, signal: controller.signal });
    const outcome = pending.then(() => 'it went through', error => error?.name);
    await soon(reached);
    controller.abort();
    assert.equal(await outcome, 'AbortError', name);
    hold.delete(name);
  }
});

test('an abort reaches a download, in the call and in the stream of its progress', async () => {
  replies.set('http.download', { json: { stream: 43 } });
  hold.add('http.download');
  let controller = new AbortController();
  const reached = new Promise(resolve => { onHold = resolve; });
  let outcome = http.download('http://127.0.0.1/f', '/tmp/f', { signal: controller.signal }).then(() => 'it went through', error => error?.name);
  await soon(reached);
  controller.abort();
  assert.equal(await outcome, 'AbortError', 'the call');
  hold.delete('http.download');

  streams.set(43, liveStream().reply);
  controller = new AbortController();
  outcome = http.download('http://127.0.0.1/f', '/tmp/f', { signal: controller.signal }).then(() => 'it went through', error => error?.name);
  while (!runtime.requests.some(request => request.url === 'native://stream/43')) await new Promise(resolve => setTimeout(resolve, 5));
  controller.abort();
  const settled = await Promise.race([outcome, new Promise(resolve => setTimeout(() => resolve('it hung'), 1000))]);
  assert.equal(settled, 'AbortError', 'the stream');
});

test('a body not read to the end is cancelled by the page, and the runtime is told to close the stream', async () => {
  const live = liveStream();
  streams.set(16, live.reply);
  const answer = (await sent('http.request', () => http.request('http://127.0.0.1/a'), { json: head({ stream: 16 }) })).result;
  const reader = answer.body.getReader();
  assert.equal(answer.body, null, 'a body is taken once');
  live.push(binaryFrame(new Uint8Array([1, 2, 3])));
  const first = await reader.read();
  assert.deepEqual([...first.value], [1, 2, 3]);
  const before = runtime.calls('runtime.stream.close').length;
  await reader.cancel();
  assert.equal(runtime.calls('runtime.stream.close').length, before + 1);
});

test('a body of exactly the size of a call goes with the call, and one byte more goes as a stream', async () => {
  replies.set('http.start', { json: { request: 6, upload: 32 } });
  replies.set('http.response', { json: head() });
  replies.set('http.request', { json: head() });
  const starts = runtime.calls('http.start').length;
  await http.request('http://127.0.0.1/a', { method: 'POST', body: new Uint8Array(192 * 1024) });
  assert.equal(runtime.calls('http.start').length, starts);
  await http.request('http://127.0.0.1/a', { method: 'POST', body: new Uint8Array(192 * 1024 + 1) });
  assert.equal(runtime.calls('http.start').length, starts + 1);
});

test('a big string goes up as its UTF-8 bytes', async () => {
  replies.set('http.start', { json: { request: 8, upload: 34 } });
  replies.set('http.response', { json: head() });
  const before = runtime.calls('runtime.stream.write').length;
  await http.request('http://127.0.0.1/up', { method: 'POST', body: 'é'.repeat(200 * 1024) });
  const written = runtime.calls('runtime.stream.write').slice(before).reduce((sum, call) => sum + call.body.length, 0);
  assert.equal(written, 400 * 1024);
});

test('a stream that fails while it is sent closes the upload and fails the request with its error', async () => {
  replies.set('http.start', { json: { request: 7, upload: 33 } });
  replies.set('http.response', { json: head() });
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
  await assert.rejects(http.request('http://127.0.0.1/a', { method: 'POST', body: broken }), /the source broke/);
  assert.equal(runtime.calls('runtime.stream.close').length, closes + 1, 'the upload was closed');
  assert.equal(runtime.calls('runtime.stream.end').length, ends, 'and not ended as if it were whole');
});

test('a signal aborted while a stream is sent stops the sending at once', async () => {
  replies.set('http.start', { json: { request: 9, upload: 35 } });
  replies.set('http.response', { json: head() });
  const controller = new AbortController();
  let pulls = 0;
  const source = new ReadableStream({
    pull(stream) {
      pulls += 1;
      if (pulls === 2) controller.abort();
      if (pulls > 4) stream.close();
      else stream.enqueue(new Uint8Array([pulls]));
    },
  });
  const before = runtime.calls('runtime.stream.write').length;
  await assert.rejects(http.request('http://127.0.0.1/a', { method: 'POST', body: source, signal: controller.signal }), { name: 'AbortError' });
  assert.equal(runtime.calls('runtime.stream.write').length - before, 1, 'nothing was sent after the abort');
});
