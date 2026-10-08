// SPDX-License-Identifier: MIT OR Apache-2.0
import assert from 'node:assert/strict';
import test from 'node:test';
import { app } from '../../../src/index.ts';
import { binaryFrame, installRuntime } from '../../fake-runtime.mjs';

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

const calls = command => runtime.calls(command).length;
const argsOf = command => runtime.argsOf(runtime.calls(command).at(-1));

test('the three standard streams are one object each, and opening one takes no arguments', async () => {
  assert.strictEqual(app.stderr, app.stderr);
  assert.strictEqual(app.stdout, app.stdout);
  assert.strictEqual(app.stdin, app.stdin);
  replies.set('app.stdin', { json: { stream: 31 } });
  replies.set('app.stdout', { json: { stream: 32 } });
  replies.set('app.stderr', { json: { stream: 33 } });
  streams.set(31, { body: new ReadableStream({ start: controller => controller.enqueue(binaryFrame(new Uint8Array([1]))) }) });
  const input = app.stdin.getReader();
  await input.read();
  input.releaseLock();
  for (const [stream, byte] of [[app.stdout, 2], [app.stderr, 3]]) {
    const writer = stream.getWriter();
    await writer.write(new Uint8Array([byte]));
    writer.releaseLock();
  }
  for (const command of ['app.stdin', 'app.stdout', 'app.stderr']) {
    assert.equal(argsOf(command), null, `${command} is asked for with no arguments`);
  }
});

test('cancelling stdin closes the stream of the runtime', async () => {
  const before = calls('runtime.stream.close');
  await app.stdin.cancel('the page has had enough');
  assert.equal(calls('runtime.stream.close'), before + 1);
});

test('aborting stdout closes the stream of the runtime, and aborting an unopened stream asks for nothing', async () => {
  const before = calls('runtime.stream.close');
  await app.stdout.abort('the page gave up');
  assert.equal(calls('runtime.stream.close'), before + 1);
  assert.equal(calls('app.stdout'), 1);
});
