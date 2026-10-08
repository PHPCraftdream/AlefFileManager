// SPDX-License-Identifier: MIT OR Apache-2.0
import './regression/core.mjs';
import assert from 'node:assert/strict';
import test from 'node:test';
import { Peer, RpcError, errors, parse } from '../src/core.ts';
import { memory, wait } from './helpers.mjs';
const envelope = (over = {}) => ({ jsonrpc: '2.0', id: 1, method: 'echo', params: {}, ...over });
test('The generic core accepts null request IDs and positional parameters without treating them as notifications.', { timeout: 2000 }, async () => {
  const peer = isolated();
  peer.handler = (_method, params) => params;
  try {
    assert.deepEqual(await peer.dispatch(envelope({ id: null, params: [1, 'two'] })), { jsonrpc: '2.0', id: null, result: [1, 'two'] });
    assert.equal(await peer.dispatch({ jsonrpc: '2.0', id: null, result: {} }), undefined);
  } finally { await peer.close(); }
});
test('Malformed notification parameters never receive a JSON-RPC reply.', { timeout: 2000 }, async () => {
  const peer = isolated(); peer.handler = () => { throw new RpcError(-32602, 'Invalid params.'); };
  try {
    for (const params of [null, 1, 'bad', [], {}]) assert.equal(await peer.dispatch({ jsonrpc: '2.0', method: 'echo', params }), undefined);
    assert.equal((await peer.dispatch(envelope({ params: 1 }))).error.code, -32602);
  } finally { await peer.close(); }
});
test('Cancellation and progress methods with IDs are rejected as requests and cannot affect active work.', { timeout: 2000 }, async () => {
  const peer = isolated(); let signal; let finish;
  peer.handler = (_method, _params, context) => { signal = context.signal; return new Promise(resolve => { finish = resolve; }); };
  const pending = peer.dispatch(envelope());
  try {
    for (const method of ['notifications/cancelled', 'notifications/progress']) {
      assert.equal((await peer.dispatch(envelope({ id: 2, method, params: { requestId: 1, progressToken: 1 } }))).error.code, -32600);
    }
    assert.equal(signal.aborted, false);
    await peer.dispatch({ jsonrpc: '2.0', method: 'notifications/cancelled', params: { requestId: 1 } });
    assert.equal(signal.aborted, true);
  } finally { finish({}); await wait(pending); await peer.close(); }
});
test('Initialize is never cancelled by an MCP cancellation notification or emitted client cancellation.', { timeout: 2000 }, async () => {
  const sent = []; const peer = new Peer({ send: async message => { sent.push(message); }, start() {}, close: async () => {} });
  let signal; let finish;
  peer.handler = (_method, _params, context) => { signal = context.signal; return new Promise(resolve => { finish = resolve; }); };
  const pending = peer.dispatch(envelope({ method: 'initialize' }));
  try {
    await peer.dispatch({ jsonrpc: '2.0', method: 'notifications/cancelled', params: { requestId: 1 } });
    assert.equal(signal.aborted, false);
    await assert.rejects(wait(peer.request('initialize', {}, { timeout: 20 })), /timed out/);
    assert.equal(sent.some(message => message.method === 'notifications/cancelled'), false);
  } finally { finish({}); await wait(pending); await peer.close(); }
});

test('Progress callbacks cannot corrupt requests and cancelled handlers cannot send further progress.', { timeout: 2000 }, async () => {
  const [left, right] = memory(); const client = new Peer(left); const server = new Peer(right);
  let context;
  server.handler = async (_method, _params, ctx) => { context = ctx; await ctx.progress(1); return {}; };
  try {
    assert.deepEqual(await wait(client.request('work', {}, { timeout: 100, onProgress: () => { throw new Error('User callback failed.'); } })), {});
    await server.close();
    // Retain a context from an active cancelled request, not from an already completed one.
    const sent = []; const peer = new Peer({ send: async message => { sent.push(message); }, start() {}, close: async () => {} }); let active; let finish;
    peer.handler = (_method, _params, ctx) => { active = ctx; return new Promise(resolve => { finish = resolve; }); };
    const pending = peer.dispatch(envelope({ params: { _meta: { progressToken: 1 } } }));
    await peer.dispatch({ jsonrpc: '2.0', method: 'notifications/cancelled', params: { requestId: 1 } });
    await wait(active.progress(2)); assert.deepEqual(sent, []); finish({}); await wait(pending); await peer.close();
    assert.ok(context);
  } finally { await client.close(); await server.close(); }
});

function isolated() { return new Peer({ send: async () => {}, start() {}, close: async () => {} }); }
test('The core returns every standard JSON-RPC error without exposing handler errors.', { timeout: 2000 }, async () => {
  const peer = isolated();
  assert.throws(() => parse('{'), error => error.code === errors.parse);
  for (const input of [null, 1, {}, envelope({ id: true })]) assert.equal((await peer.dispatch(input)).error.code, errors.invalidRequest);
  assert.equal((await peer.dispatch(envelope())).error.code, errors.methodNotFound);
  peer.handler = () => { throw new RpcError(errors.invalidParams, 'Invalid arguments.'); };
  assert.equal((await peer.dispatch(envelope())).error.code, errors.invalidParams);
  peer.handler = () => { throw new Error('secret'); };
  assert.deepEqual((await peer.dispatch(envelope())).error, { code: errors.internal, message: 'Internal error.' });
  assert.equal((await peer.dispatch(envelope({ id: undefined }))).error.code, errors.invalidRequest);
  await peer.close();
});
test('Notifications have no response and only the non-negotiated generic March compatibility mode permits batches.', { timeout: 2000 }, async () => {
  const peer = isolated(); peer.handler = () => ({ ok: true });
  const notification = { jsonrpc: '2.0', method: 'echo' };
  assert.equal(await peer.dispatch(notification), undefined);
  assert.equal((await peer.dispatch([envelope()])).error.code, -32600);
  peer.revision = '2025-03-26';
  assert.deepEqual(await peer.dispatch([notification, envelope()]), [{ jsonrpc: '2.0', id: 1, result: { ok: true } }]);
  assert.equal(await peer.dispatch([notification]), undefined);
  assert.equal((await peer.dispatch([])).error.code, -32600);
  await peer.close();
});
test('Requests report progress and cancellation aborts the server context safely.', { timeout: 2000 }, async () => {
  const [left, right] = memory(); const client = new Peer(left); const server = new Peer(right);
  let aborted;
  const cancellation = new Promise(resolve => { aborted = resolve; });
  server.handler = async (_method, params, context) => {
    await context.progress(1, 2, 'Half done.');
    if (params.hang) {
      if (context.signal.aborted) { aborted(); return {}; }
      return new Promise(resolve => context.signal.addEventListener('abort', () => { aborted(); resolve({}); }, { once: true }));
    }
    return { ok: true };
  };
  const progress = [];
  assert.deepEqual(await wait(client.request('work', {}, { timeout: 100, onProgress: item => progress.push(item) })), { ok: true });
  assert.equal(progress[0].progress, 1);
  const controller = new AbortController();
  const pending = client.request('work', { hang: true }, { timeout: 100, signal: controller.signal });
  controller.abort(new Error('Stopped.'));
  await assert.rejects(wait(pending), /Stopped/); await wait(cancellation);
  await assert.rejects(wait(client.request('work', { hang: true }, { timeout: 20 })), /timed out/);
  await client.close(); await server.close();
  await assert.rejects(client.request('work'), /closed/);
});
test('Closing a peer rejects pending requests and pre-aborted requests are not sent.', { timeout: 2000 }, async () => {
  const peer = isolated();
  const controller = new AbortController(); controller.abort();
  await assert.rejects(peer.request('x', {}, { signal: controller.signal }));
  const pending = peer.request('x', {}, { timeout: 100 });
  await peer.close(); await assert.rejects(pending, /closed/);
  assert.throws(() => isolated().request('x', {}, { timeout: 0 }), /timeout/);
});
