// SPDX-License-Identifier: MIT OR Apache-2.0
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { AlefError } from '../../../src/core/errors.ts';
import { audio, camera, microphone } from '../../../src/system/media.ts';
import { binaryFrame, endFrame, installRuntime, join, jsonFrame, liveStream } from '../../fake-runtime.mjs';

// command name -> reply; stream id -> reply.
const replies = new Map();
const streams = new Map();
const runtime = installRuntime({
  handler(request) {
    const call = /^native:\/\/call\/(.+)$/.exec(request.url);
    if (call) return replies.get(call[1]);
    const match = /^native:\/\/stream\/(\d+)$/.exec(request.url);
    if (match) return streams.get(Number(match[1])) ?? { chunks: [endFrame()] };
    return undefined;
  },
});

/** Rejects with `label` when `promise` takes longer than 2000 ms; never leaves a dangling timer. */
async function bounded(promise, label = 'the test timed out') {
  let timer;
  try {
    return await Promise.race([promise, new Promise((_, reject) => { timer = setTimeout(() => reject(new Error(label)), 2000); })]);
  } finally {
    clearTimeout(timer);
  }
}

async function collect(iterable) {
  const items = [];
  for await (const item of iterable) items.push(item);
  return items;
}

test('camera list returns the devices the runtime reported', async () => {
  const devices = [{ id: 'virtual-0', label: 'Virtual Camera', width: 640, height: 480, fps: 30 }];
  replies.set('media.camera.list', { json: { devices } });
  assert.deepEqual(await bounded(camera.list()), devices);
});

test('camera open pairs every header with its payload and reports the frames', async () => {
  const stream = liveStream();
  streams.set(71, stream.reply);
  replies.set('media.camera.open', { json: { camera: 11, stream: 71 } });
  const bytes = new Uint8Array(16);
  stream.push(join(
    jsonFrame({ seq: 1, timestamp: 111, width: 2, height: 2, dropped: 0, byteLength: 16, format: 'rgba' }),
    binaryFrame(bytes),
    jsonFrame({ seq: 2, timestamp: 112, width: 2, height: 2, dropped: 0, byteLength: 16, format: 'rgba' }),
    binaryFrame(bytes),
  ));
  stream.end();
  const cam = await bounded(camera.open('virtual-0'));
  assert.equal(cam.id, 11);
  assert.equal(cam.stream, 71);
  const frames = await bounded(collect(cam));
  assert.deepEqual(frames, [
    { seq: 1, timestamp: 111, width: 2, height: 2, dropped: 0, byteLength: 16, format: 'rgba', data: bytes },
    { seq: 2, timestamp: 112, width: 2, height: 2, dropped: 0, byteLength: 16, format: 'rgba', data: bytes },
  ]);
});

test('a frame larger than one transport chunk is reassembled from its pieces', async () => {
  const stream = liveStream();
  streams.set(90, stream.reply);
  replies.set('media.camera.open', { json: { camera: 20, stream: 90 } });
  const pieces = [1, 2, 3].map(value => new Uint8Array(1000).fill(value));
  stream.push(join(
    jsonFrame({ seq: 1, timestamp: 9, width: 25, height: 30, dropped: 0, byteLength: 3000, format: 'rgba' }),
    ...pieces.map(binaryFrame),
  ));
  stream.end();
  const cam = await bounded(camera.open('virtual-0'));
  const frames = await bounded(collect(cam));
  assert.equal(frames.length, 1, 'three chunks are one frame');
  assert.equal(frames[0].data.length, 3000);
  // Distinct pieces: a decoder that copies every piece to offset zero fails these.
  assert.deepEqual(
    [frames[0].data[0], frames[0].data[999], frames[0].data[1000], frames[0].data[1999], frames[0].data[2000], frames[0].data[2999]],
    [1, 1, 2, 2, 3, 3],
  );
});

test('a payload cut short by the end of the stream is an internal error', async () => {
  const stream = liveStream();
  streams.set(91, stream.reply);
  replies.set('media.camera.open', { json: { camera: 21, stream: 91 } });
  const piece = new Uint8Array(1000);
  stream.push(join(
    jsonFrame({ seq: 1, timestamp: 9, width: 30, height: 250, dropped: 0, byteLength: 3000, format: 'rgba' }),
    binaryFrame(piece), binaryFrame(piece),
  ));
  stream.end();
  const cam = await bounded(camera.open('virtual-0'));
  await assert.rejects(bounded(collect(cam)), error => error instanceof AlefError && error.code === 'INTERNAL');
});

test('a payload without a header is an internal error', async () => {
  const stream = liveStream();
  streams.set(74, stream.reply);
  replies.set('media.camera.open', { json: { camera: 14, stream: 74 } });
  stream.push(binaryFrame(new Uint8Array(16)));
  stream.end();
  const cam = await bounded(camera.open('virtual-0'));
  await assert.rejects(bounded(collect(cam)), error => error instanceof AlefError && error.code === 'INTERNAL');
});

test('closing a camera closes the stream and calls media.camera.close', async () => {
  const stream = liveStream();
  streams.set(76, stream.reply);
  replies.set('media.camera.open', { json: { camera: 15, stream: 76 } });
  stream.push(join(
    jsonFrame({ seq: 1, timestamp: 1, width: 1, height: 1, dropped: 0, byteLength: 4, format: 'rgba' }),
    binaryFrame(new Uint8Array(4)),
  ));
  const cam = await bounded(camera.open('virtual-0'));
  const iterator = cam[Symbol.asyncIterator]();
  const frame = await bounded(iterator.next());
  assert.equal(frame.value.seq, 1);
  await bounded(iterator.return?.());
  await bounded(cam.close());
  const streamCloses = runtime.calls('runtime.stream.close').map(runtime.argsOf).filter(args => args.id === 76);
  assert.deepEqual(streamCloses, [{ id: 76 }]);
  assert.deepEqual(runtime.calls('media.camera.close').map(runtime.argsOf), [{ camera: 15 }]);
});

test('microphone open hands out f32 chunks', async () => {
  const stream = liveStream();
  streams.set(78, stream.reply);
  replies.set('media.microphone.open', { json: { microphone: 16, stream: 78 } });
  const samples = new Float32Array([1, 3]);
  stream.push(join(
    jsonFrame({ seq: 1, timestamp: 5, dropped: 0, byteLength: 8, rate: 48000, samples: 2, format: 'f32le' }),
    binaryFrame(new Uint8Array(samples.buffer)),
  ));
  stream.end();
  const mic = await bounded(microphone.open('virtual-0'));
  assert.equal(mic.id, 16);
  const chunks = await bounded(collect(mic));
  assert.equal(chunks.length, 1, 'the iteration ended by itself');
  assert.equal(chunks[0].rate, 48000);
  assert.deepEqual(chunks[0].data, new Float32Array([1, 3]));
  await bounded(mic.close());
});

test('audio open writes raw bytes into the stream and close reports the stats', async () => {
  replies.set('audio.open', { json: { sink: 17, stream: 73 } });
  replies.set('audio.close', { json: { samples: 1, checksum: 'aabbccdd11223344' } });
  const sink = await bounded(audio.open());
  assert.equal(sink.id, 17);
  const before = runtime.calls('runtime.stream.write').length;
  await bounded(sink.write(new Float32Array([1])));
  await bounded(sink.write(new Float32Array([2])));
  const writes = runtime.calls('runtime.stream.write').slice(before);
  assert.equal(writes.length, 2, 'both writes go through the one stream the open created');
  assert.deepEqual(writes.map(request => [...request.body]), [[0, 0, 128, 63], [0, 0, 0, 64]]);
  assert.deepEqual(writes.map(runtime.argsOf), [{ id: 73 }, { id: 73 }]);
  const beforeEnd = runtime.calls('runtime.stream.end').length;
  const stats = await bounded(sink.close());
  assert.deepEqual(runtime.calls('runtime.stream.end').slice(beforeEnd).map(runtime.argsOf), [{ id: 73 }]);
  assert.deepEqual(runtime.calls('audio.close').map(runtime.argsOf), [{ sink: 17 }]);
  assert.deepEqual(stats, { samples: 1, checksum: 'aabbccdd11223344' });
});

test('camera open rejects an out-of-range fps before calling the runtime', async () => {
  const before = runtime.calls('media.camera.open').length;
  await assert.rejects(bounded(camera.open('virtual-0', { fps: 0 })), error => error instanceof AlefError && error.code === 'INVALID_ARGUMENT');
  await assert.rejects(bounded(camera.open('virtual-0', { fps: Number.NaN })), error => error instanceof AlefError && error.code === 'INVALID_ARGUMENT');
  assert.equal(runtime.calls('media.camera.open').length, before, 'no media.camera.open call left the segment');
});
