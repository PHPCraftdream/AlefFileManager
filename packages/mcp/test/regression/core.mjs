// SPDX-License-Identifier: MIT OR Apache-2.0
import assert from 'node:assert/strict';
import test from 'node:test';
import { Peer, RpcError } from '../../src/core.ts';
import { wait } from '../helpers.mjs';
const request = { jsonrpc: '2.0', id: 7, method: 'work', params: {} };
const isolated = () => new Peer({ send: async () => {}, start() {}, close: async () => {} });

test('Duplicate active IDs are rejected without invoking or replacing the original handler.', { timeout: 2000 }, async () => {
  const peer = isolated(); let finish; let calls = 0;
  peer.handler = () => { calls++; return new Promise(resolve => { finish = resolve; }); };
  const original = peer.dispatch(request);
  try {
    const duplicate = peer.dispatch(request);
    assert.equal(calls, 1);
    const reply = await wait(duplicate);
    assert.equal(reply.error.code, -32600);
  } finally { peer.end(); finish({}); await wait(original); await peer.close(); }
});

test('String parameters are rejected before handlers, including silent notifications.', async () => {
  const peer = isolated(); let calls = 0;
  peer.handler = () => { calls++; return {}; };
  try {
    assert.equal((await peer.dispatch({ ...request, params: 'poison' })).error.code, -32602);
    assert.equal(await peer.dispatch({ jsonrpc: '2.0', method: 'work', params: 'poison' }), undefined);
    assert.equal(calls, 0);
  } finally { await peer.close(); }
});

test('Ending a peer aborts running handlers with the connection reason.', { timeout: 2000 }, async () => {
  const peer = isolated(); let signal; let finish;
  peer.handler = (_method, _params, context) => { signal = context.signal; return new Promise(resolve => { finish = resolve; }); };
  const pending = peer.dispatch(request); const reason = new Error('Connection lost.');
  try {
    peer.end(reason);
    assert.equal(signal.aborted, true); assert.equal(signal.reason, reason);
  } finally { finish({}); await wait(pending); await peer.close(); }
});

test('Malformed error responses reject with sanitized RpcErrors, never remote poison.', { timeout: 2000 }, async () => {
  for (const error of [{ code: 'poison', message: 'remote secret' }, { code: -123, message: { secret: true } }]) {
    let sent;
    const peer = new Peer({ send: async message => { sent = message; }, start() {}, close: async () => {} });
    try {
      const pending = peer.request('work', {}, { timeout: 100 });
      const rejected = assert.rejects(pending, value => {
        assert.ok(value instanceof RpcError); assert.equal(value.code, -32600);
        assert.equal(value.message, 'Invalid response.'); assert.equal(value.data, undefined); return true;
      });
      await peer.dispatch({ jsonrpc: '2.0', id: sent.id, error });
      await rejected;
    } finally { await peer.close(); }
  }
});
