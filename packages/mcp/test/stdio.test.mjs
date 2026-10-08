// SPDX-License-Identifier: MIT OR Apache-2.0
import './regression/stdio.mjs';
import './regression/console.mjs';
import assert from 'node:assert/strict';
import test from 'node:test';
import { mcp } from '../src/index.ts';
import { stdio } from '../src/transports/stdio.ts';
import { runtime } from '../src/transports/runtime.ts';
import { encode, initialize, wait } from './helpers.mjs';
test('Stdio handles fragmented UTF-8, CRLF and multiple newline messages and negotiates the newest revision.', { timeout: 2000 }, async () => {
  const output = []; let input; let first; let second;
  const firstReply = new Promise(resolve => { first = resolve; });
  const secondReply = new Promise(resolve => { second = resolve; });
  const readable = new ReadableStream({ start(controller) { input = controller; } });
  const writable = new WritableStream({ write(value) { output.push(new TextDecoder().decode(value)); if (output.length === 1) first(); if (output.length === 2) second(); } });
  const server = mcp.server({ name: 'stdio', version: '1' });
  await server.listen({ stdio: { input: readable, output: writable } });
  const text = JSON.stringify(initialize('2025-11-25')) + '\r\n';
  const bytes = encode(text);
  input.enqueue(bytes.subarray(0, 10)); input.enqueue(bytes.subarray(10));
  // Wait for initialization before sending the initialized notification.
  await wait(firstReply);
  input.enqueue(encode('{"jsonrpc":"2.0","method":"notifications/initialized"}\r\n{"jsonrpc":"2.0","id":2,"method":"tools/list"}\n'));
  await wait(secondReply);
  assert.equal(JSON.parse(output[0]).result.protocolVersion, '2025-11-25');
  assert.deepEqual(JSON.parse(output[1]).result, { tools: [] });
  input.close(); await server.close();
});
test('Stdio reports parse errors and incomplete or oversized EOF input without hanging.', { timeout: 2000 }, async () => {
  for (const text of ['{\n', '{', 'x'.repeat(20)]) {
    const output = [];
    let end;
    const ended = new Promise(resolve => { end = resolve; });
    const transport = stdio({ readable: new ReadableStream({ start(controller) { controller.enqueue(encode(text)); controller.close(); } }), writable: new WritableStream({ write(bytes) { output.push(new TextDecoder().decode(bytes)); } }) }, 10);
    transport.start(() => {}, error => end(error));
    const error = await wait(ended);
    if (text === '{\n') { assert.equal(JSON.parse(output[0]).error.code, -32700); assert.equal(error, undefined); }
    else assert.match(error.message, /incomplete|too large/);
    await transport.close();
  }
});
test('Stdio decodes multibyte characters split across chunks and rejects writes after close.', { timeout: 2000 }, async () => {
  const bytes = encode('{"text":"😀"}\r\n{"text":"two"}\n');
  const received = []; let finish;
  const ended = new Promise(resolve => { finish = resolve; });
  const transport = stdio({ readable: new ReadableStream({ start(controller) { for (const byte of bytes) controller.enqueue(new Uint8Array([byte])); controller.close(); } }), writable: new WritableStream() });
  transport.start(message => received.push(message), finish);
  await wait(ended); assert.deepEqual(received, [{ text: '😀' }, { text: 'two' }]);
  await transport.close(); await assert.rejects(transport.send({}), /closed/);
});
test('The command client passes literal arguments to cli.spawn and kills and waits on close.', { timeout: 2000 }, async () => {
  const toServer = new TransformStream(); const toClient = new TransformStream();
  const server = mcp.server({ name: 'child', version: '1' });
  await server.listen({ stdio: { readable: toServer.readable, writable: toClient.writable } });
  let spawned; let killed = 0; let waited = 0;
  const original = runtime.cli.spawn;
  runtime.cli.spawn = async (command, args, options) => {
    spawned = { command, args, options };
    return { stdin: toServer.writable, stdout: toClient.readable, async kill() { killed++; }, async wait() { waited++; } };
  };
  try {
    const client = await wait(mcp.connect({ command: 'mock', args: ['literal;argument'], timeout: 100 }));
    assert.equal(spawned.command, 'mock'); assert.deepEqual(spawned.args, ['literal;argument']); assert.equal(spawned.options.stderr, 'ignore');
    assert.deepEqual(await client.listTools(), { tools: [] });
    await wait(client.close()); assert.equal(killed, 1); assert.equal(waited, 1);
  } finally { runtime.cli.spawn = original; await server.close(); }
});
