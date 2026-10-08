// SPDX-License-Identifier: MIT OR Apache-2.0
import assert from 'node:assert/strict';
import test from 'node:test';
import { stdio } from '../../src/transports/stdio.ts';
import { encode, wait } from '../helpers.mjs';

test('A newline-terminated oversized line ends input without delivering a message or parse reply.', { timeout: 2000 }, async () => {
  const received = []; const output = []; let end;
  const ended = new Promise(resolve => { end = resolve; });
  const transport = stdio({
    readable: new ReadableStream({ start(controller) { controller.enqueue(encode(JSON.stringify({ text: 'x'.repeat(40) }) + '\n')); controller.close(); } }),
    writable: new WritableStream({ write(bytes) { output.push(bytes); } }),
  }, 20);
  try {
    transport.start(message => received.push(message), end);
    const error = await wait(ended);
    assert.match(error?.message ?? '', /too large/); assert.deepEqual(received, []); assert.deepEqual(output, []);
  } finally { await transport.close(); }
});

test('Oversized output rejects before writing bytes, while a boundary-size message succeeds.', async () => {
  const output = [];
  const transport = stdio({ readable: new ReadableStream(), writable: new WritableStream({ write(bytes) { output.push(bytes); } }) }, 10);
  try {
    await assert.rejects(transport.send({ text: '😀😀' }), /too large/); assert.equal(output.length, 0);
    await transport.send({ x: 'a' }); assert.equal(output[0].byteLength, 10);
  } finally { await transport.close(); }
});

test('Blank and whitespace-only lines are silently skipped between valid messages.', { timeout: 2000 }, async () => {
  const received = []; const output = []; let end;
  const ended = new Promise(resolve => { end = resolve; });
  const transport = stdio({
    readable: new ReadableStream({ start(controller) { controller.enqueue(encode('\n  \r\n{"x":1}\n\t\n{"x":2}\n')); controller.close(); } }),
    writable: new WritableStream({ write(bytes) { output.push(bytes); } }),
  });
  try {
    transport.start(message => received.push(message), end);
    assert.equal(await wait(ended), undefined); assert.deepEqual(received, [{ x: 1 }, { x: 2 }]); assert.deepEqual(output, []);
  } finally { await transport.close(); }
});
