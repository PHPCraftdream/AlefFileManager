// SPDX-License-Identifier: MIT OR Apache-2.0
import assert from 'node:assert/strict';
import test from 'node:test';
import { AlefError, openReadable, openWritable } from '../src/index.ts';
import {
  binaryFrame, chunked, endFrame, errorFrame, frame, installRuntime, join, jsonFrame, quiet,
} from './fake-runtime.mjs';

// stream id -> reply; control calls are answered with {}.
const streams = new Map();
const runtime = installRuntime({
  handler(request) {
    const match = /^native:\/\/stream\/(\d+)$/.exec(request.url);
    if (!match) return undefined;
    return streams.get(Number(match[1])) ?? { status: 404, json: { code: 'NOT_FOUND', message: 'stream not found' } };
  },
});
const sent = name => runtime.calls(name).map(runtime.argsOf);

async function collect(readable) {
  const frames = [];
  for await (const item of readable) frames.push(item);
  return frames;
}

test('json and binary frames decode identically whatever the chunk boundaries', async () => {
  const wire = join(jsonFrame({ a: 'привет' }), binaryFrame(new Uint8Array([1, 2, 3])), jsonFrame([1]), endFrame());
  const expected = [
    { kind: 'json', value: { a: 'привет' } },
    { kind: 'binary', data: new Uint8Array([1, 2, 3]) },
    { kind: 'json', value: [1] },
  ];
  for (const size of [1, 2, 3, 5, 7, wire.length]) {
    streams.set(1, { chunks: chunked(wire, size) });
    const frames = await collect(await openReadable(1));
    assert.deepEqual(frames, expected, `chunk size ${size}`);
  }
  assert.equal(runtime.calls('runtime.stream.close').length, 0, 'a stream that ended by itself is not closed again');
});

test('an error frame ends the iteration with the runtime error', async () => {
  streams.set(2, { chunks: [join(jsonFrame(1), errorFrame({ code: 'BUSY', message: 'event subscriber too slow' }))] });
  const readable = await openReadable(2);
  const seen = [];
  const error = await (async () => {
    try {
      for await (const item of readable) seen.push(item);
    } catch (reason) {
      return reason;
    }
    return undefined;
  })();
  assert.deepEqual(seen, [{ kind: 'json', value: 1 }]);
  assert.ok(error instanceof AlefError);
  assert.equal(error.code, 'BUSY');
});

test('consumed bytes are acked in batches, json frames included', async () => {
  const piece = new Uint8Array(100 * 1024);
  const note = { text: 'x'.repeat(50) };
  const noteBytes = JSON.stringify(note).length;
  streams.set(3, { chunks: [join(binaryFrame(piece), binaryFrame(piece), jsonFrame(note), binaryFrame(piece), endFrame())] });
  const before = runtime.calls('runtime.stream.ack').length;
  await collect(await openReadable(3));
  const acks = sent('runtime.stream.ack').slice(before);
  const total = 3 * piece.length + noteBytes;
  assert.deepEqual(acks, [{ id: 3, bytes: total }], 'one ack once 256 KiB were consumed; it counts every payload byte');
});

test('a reader that stays under the batch size acks nothing yet', async () => {
  streams.set(4, { chunks: [join(binaryFrame(new Uint8Array(1000)), endFrame())] });
  const before = runtime.calls('runtime.stream.ack').length;
  await collect(await openReadable(4));
  assert.equal(runtime.calls('runtime.stream.ack').length, before);
});

test('leaving the loop early aborts the request and closes the stream in the runtime', async () => {
  streams.set(5, { chunks: [join(jsonFrame(1), jsonFrame(2), jsonFrame(3))] });
  const readable = await openReadable(5);
  for await (const item of readable) {
    assert.deepEqual(item, { kind: 'json', value: 1 });
    break;
  }
  const request = runtime.requests.findLast(item => item.url === 'native://stream/5');
  assert.equal(request.signal.aborted, true);
  assert.deepEqual(sent('runtime.stream.close').filter(args => args.id === 5), [{ id: 5 }]);
  await readable.close();
  assert.equal(sent('runtime.stream.close').filter(args => args.id === 5).length, 1, 'close is idempotent');
});

test('aborting the signal closes the stream', async () => {
  streams.set(6, { chunks: [jsonFrame(1)] });
  const controller = new AbortController();
  await openReadable(6, { signal: controller.signal });
  controller.abort();
  await quiet(runtime);
  assert.deepEqual(sent('runtime.stream.close').filter(args => args.id === 6), [{ id: 6 }]);
  await assert.rejects(openReadable(6, { signal: controller.signal }), { name: 'AbortError' });
});

test('an unknown stream is a NOT_FOUND error and a corrupt length is refused', async () => {
  await assert.rejects(openReadable(99), error => error instanceof AlefError && error.code === 'NOT_FOUND' && error.status === 404);
  const corrupt = new Uint8Array(5);
  corrupt[0] = 2;
  new DataView(corrupt.buffer).setUint32(1, 16 * 1024 * 1024 + 1, true);
  streams.set(7, { chunks: [corrupt] });
  const readable = await openReadable(7);
  await assert.rejects(collect(readable), error => error instanceof AlefError && error.code === 'INTERNAL');
});

test('an unknown frame kind is an error, not silently skipped', async () => {
  streams.set(8, { chunks: [frame(9, new Uint8Array([1]))] });
  await assert.rejects(collect(await openReadable(8)), error => error instanceof AlefError && error.code === 'INTERNAL');
});

test('a writable splits large chunks to the runtime chunk size and keeps the order', async () => {
  const writable = await openWritable(70);
  const before = runtime.calls('runtime.stream.write').length;
  await writable.write(new Uint8Array([1, 2, 3, 4, 5, 6, 7, 8, 9, 10]));
  const writes = runtime.calls('runtime.stream.write').slice(before);
  assert.deepEqual(writes.map(request => [...request.body]), [[1, 2, 3, 4], [5, 6, 7, 8], [9, 10]]);
  for (const request of writes) {
    assert.deepEqual(runtime.argsOf(request), { id: 70 });
    assert.equal(request.headers['content-type'], 'application/octet-stream');
  }
  await writable.end();
  assert.deepEqual(sent('runtime.stream.end'), [{ id: 70 }]);
  await writable.abort();
  assert.deepEqual(sent('runtime.stream.close').filter(args => args.id === 70), [{ id: 70 }]);
});
