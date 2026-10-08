// SPDX-License-Identifier: MIT OR Apache-2.0
import './regression/methods.mjs';
import assert from 'node:assert/strict';
import test from 'node:test';
import { mcp } from '../src/index.ts';
import { runtime } from '../src/transports/runtime.ts';
import { memory, wait } from './helpers.mjs';
const info = { name: 'test', version: '1' };
test('The fluent server and all six client methods work through an in-memory transport.', { timeout: 3000 }, async () => {
  const [left, right] = memory();
  const server = mcp.server(info)
    .tool('echo', { description: 'Echo.', inputSchema: { type: 'object', properties: { text: { type: 'string' } }, required: ['text'], additionalProperties: false } }, args => ({ content: [{ type: 'text', text: args.text }] }))
    .resource('test://one', { name: 'One', mimeType: 'text/plain' }, args => ({ contents: [{ uri: args.uri, text: 'resource' }] }))
    .prompt('hello', { arguments: [{ name: 'name', required: true }] }, args => ({ messages: [{ role: 'user', content: { type: 'text', text: args.name } }] }));
  assert.equal(await server.listen({ transport: right }), server);
  const client = await wait(mcp.connect({ transport: left, timeout: 200 }));
  assert.deepEqual(client.serverInfo, info);
  assert.equal((await client.listTools()).tools[0].name, 'echo');
  assert.equal((await client.callTool('echo', { text: 'hello' })).content[0].text, 'hello');
  assert.equal((await client.listResources()).resources[0].uri, 'test://one');
  assert.equal((await client.readResource('test://one')).contents[0].text, 'resource');
  assert.equal((await client.listPrompts()).prompts[0].name, 'hello');
  assert.equal((await client.getPrompt('hello', { name: 'friend' })).messages[0].content.text, 'friend');
  for (const pending of [client.callTool('echo', {}), client.callTool('missing'), client.readResource('missing'), client.getPrompt('hello'), client.getPrompt('missing'), client.peer.request('missing', {}, { timeout: 100 })]) await assert.rejects(pending, error => [-32601, -32602].includes(error.code));
  await client.close(); await client.close(); await server.close(); await server.close();
});
test('Negotiation accepts both revisions and falls back to the newest revision.', { timeout: 3000 }, async () => {
  for (const version of ['2025-11-25', '2025-06-18', '2025-03-26', 'unknown']) {
    const [left, right] = memory(); const server = mcp.server(info); await server.listen({ transport: right });
    const client = await wait(mcp.connect({ transport: left, protocolVersion: version, timeout: 100 }));
    assert.equal(client.peer.revision, ['unknown', '2025-03-26'].includes(version) ? '2025-11-25' : version);
    await client.close(); await server.close();
  }
});
test('The server rejects calls before the initialized notification and is closed for good.', { timeout: 2000 }, async () => {
  const server = mcp.server(info);
  const peer = server.attach({ send: async () => {}, start() {}, close: async () => {} });
  const result = await peer.dispatch({ jsonrpc: '2.0', id: 1, method: 'tools/list' });
  assert.equal(result.error.code, -32600);
  assert.equal((await peer.dispatch({ jsonrpc: '2.0', id: 2, method: 'initialize', params: [] })).error.code, -32602);
  assert.equal(await peer.dispatch({ jsonrpc: '2.0', method: 'initialize', params: [] }), undefined);
  await server.close(); await assert.rejects(server.listen({ transport: memory()[0] }), /closed/);
});
test('Connection and initialization timeouts reject without retaining pending work.', { timeout: 2000 }, async () => {
  let closed = 0;
  await assert.rejects(wait(mcp.connect({ transport: { send: async () => {}, start() {}, close: async () => { closed++; } }, timeout: 20 })), /timed out/);
  assert.equal(closed, 1);
  const original = runtime.cli.spawn;
  runtime.cli.spawn = async () => new Promise(() => {});
  try { await assert.rejects(wait(mcp.connect({ command: 'mock', args: ['one'], timeout: 20 })), /timed out/); }
  finally { runtime.cli.spawn = original; }
});
