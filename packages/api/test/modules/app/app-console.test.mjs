// SPDX-License-Identifier: MIT OR Apache-2.0
import assert from 'node:assert/strict';
import test from 'node:test';
import { AlefError, app } from '../../../src/index.ts';
import { binaryFrame, endFrame, installRuntime, join } from '../../fake-runtime.mjs';

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
const decode = bytes => new TextDecoder().decode(bytes);
const argsOf = command => runtime.argsOf(runtime.calls(command).at(-1));
const calls = command => runtime.calls(command).length;
const written = since => runtime.calls('runtime.stream.write').slice(since).map(call => decode(call.body)).join('');

test('stdin is one stream that asks the runtime for the input when it is first read, and gives it whole', async () => {
  replies.set('app.stdin', { json: { stream: 11 } });
  streams.set(11, { chunks: [join(binaryFrame(encode('hel')), binaryFrame(encode('lo\n')), endFrame())] });
  assert.strictEqual(app.stdin, app.stdin, 'one stream, however often it is asked for');
  assert.equal(calls('app.stdin'), 0, 'nothing is asked for until the stream is read');
  const reader = app.stdin.getReader();
  const pieces = [];
  for (;;) {
    const { done, value } = await reader.read();
    if (done) break;
    pieces.push(decode(value));
  }
  assert.equal(pieces.join(''), 'hello\n');
  assert.equal(calls('app.stdin'), 1);
});

test('stdout and stderr are streams of their own that are opened by the first write and ended by the close', async () => {
  replies.set('app.stdout', { json: { stream: 21 } });
  replies.set('app.stderr', { json: { stream: 22 } });
  assert.strictEqual(app.stdout, app.stdout);
  assert.notStrictEqual(app.stdout, app.stderr);
  assert.equal(calls('app.stdout'), 0, 'nothing is asked for until something is written');
  const before = calls('runtime.stream.write');
  const out = app.stdout.getWriter();
  await out.write(encode('one '));
  await out.write(encode('two'));
  assert.equal(calls('app.stdout'), 1, 'one stream for all the writes');
  assert.equal(written(before), 'one two');
  const ends = calls('runtime.stream.end');
  await out.close();
  assert.equal(calls('runtime.stream.end'), ends + 1, 'closing ends the stream');

  const mark = calls('runtime.stream.write');
  const err = app.stderr.getWriter();
  await err.write(encode('oops'));
  assert.equal(calls('app.stderr'), 1);
  assert.equal(written(mark), 'oops');
});

test('exit quits with the code and never comes back', async () => {
  const outcome = await Promise.race([app.exit(3).then(() => 'came back'), new Promise(resolve => setTimeout(() => resolve('pending'), 300))]);
  assert.equal(outcome, 'pending');
  assert.deepEqual(argsOf('app.quit'), { code: 3 });
  await Promise.race([app.exit(), new Promise(resolve => setTimeout(resolve, 100))]);
  assert.deepEqual(argsOf('app.quit'), {}, 'no code, no field');
});

test('exit fails when the runtime refuses the quit', async () => {
  replies.set('app.quit', { status: 400, json: { code: 'INVALID_ARGUMENT', message: 'code must be an integer from 0 to 255' } });
  await assert.rejects(app.exit(300), error => error instanceof AlefError && error.code === 'INVALID_ARGUMENT');
  replies.delete('app.quit');
});
