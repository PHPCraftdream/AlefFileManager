// SPDX-License-Identifier: MIT OR Apache-2.0
import assert from 'node:assert/strict';
import test from 'node:test';
import { mcp } from '../../src/index.ts';
import { memory, initialize, wait } from '../helpers.mjs';
const info = { name: 'guards', version: '1' };
const call = (method, params = {}) => ({ jsonrpc: '2.0', id: 2, method, params });

test('Initialization is single-use and an early initialized notification does not make a session ready.', async () => {
  const server = mcp.server(info);
  const peer = server.attach({ send: async () => {}, start() {}, close: async () => {} });
  try {
    assert.equal(await peer.dispatch({ jsonrpc: '2.0', method: 'notifications/initialized' }), undefined);
    assert.equal((await peer.dispatch(call('tools/list'))).error.code, -32600);
    assert.ok((await peer.dispatch(initialize())).result);
    assert.equal((await peer.dispatch(initialize())).error.code, -32600);
    await peer.dispatch({ jsonrpc: '2.0', method: 'notifications/initialized' });
    assert.deepEqual((await peer.dispatch(call('tools/list'))).result, { tools: [] });
    assert.equal((await peer.dispatch(initialize())).error.code, -32600);
  } finally { await server.close(); }
});

test('Initialize requires capabilities and clientInfo without advancing the phase after invalid parameters.', async () => {
  const server = mcp.server(info); const peer = server.attach({ send: async () => {}, start() {}, close: async () => {} });
  try {
    for (const field of ['capabilities', 'clientInfo']) {
      const message = initialize(); delete message.params[field];
      assert.equal((await peer.dispatch(message)).error.code, -32602, field);
    }
    assert.ok((await peer.dispatch(initialize())).result);
  } finally { await server.close(); }
});

test('Ready sessions reject pagination cursors and numeric prompt arguments before invoking handlers.', async () => {
  let calls = 0;
  const server = mcp.server(info).prompt('hello', { arguments: [{ name: 'name', required: true }] }, () => { calls++; return { messages: [] }; });
  const peer = server.attach({ send: async () => {}, start() {}, close: async () => {} });
  try {
    await peer.dispatch(initialize()); await peer.dispatch({ jsonrpc: '2.0', method: 'notifications/initialized' });
    for (const method of ['tools/list', 'resources/list', 'prompts/list']) assert.equal((await peer.dispatch(call(method, { cursor: 'next' }))).error.code, -32602);
    assert.equal((await peer.dispatch(call('prompts/get', { name: 'hello', arguments: { name: 42 } }))).error.code, -32602);
    assert.equal(calls, 0);
    assert.deepEqual((await peer.dispatch(call('prompts/get', { name: 'hello', arguments: { name: '42' } }))).result, { messages: [] });
    assert.equal(calls, 1);
  } finally { await server.close(); }
});

test('A second listen fails before starting another transport.', async () => {
  const server = mcp.server(info); let started = 0;
  try {
    await server.listen({ transport: memory()[0] });
    await assert.rejects(server.listen({ transport: { send: async () => {}, start() { started++; }, close: async () => {} } }), /already listening/);
    assert.equal(started, 0);
  } finally { await server.close(); }
});

test('Connect rejects unsupported revisions and absent serverInfo and closes failed connections.', { timeout: 2000 }, async () => {
  for (const result of [
    { protocolVersion: 'unsupported', capabilities: {}, serverInfo: info },
    { protocolVersion: '2025-11-25', capabilities: {} },
  ]) {
    let receive; let closed = 0; const sent = [];
    const transport = {
      start(callback) { receive = callback; },
      async send(message) { sent.push(message); if (message.method === 'initialize') receive({ jsonrpc: '2.0', id: message.id, result }); },
      async close() { closed++; },
    };
    await assert.rejects(wait(mcp.connect({ transport, timeout: 100 })), /unsupported initialization response/);
    assert.equal(closed, 1); assert.deepEqual(sent.map(message => message.method), ['initialize']);
  }
});
