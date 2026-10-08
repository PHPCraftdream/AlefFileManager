// SPDX-License-Identifier: MIT OR Apache-2.0
import assert from 'node:assert/strict';
import test from 'node:test';
import { McpServer } from '../../src/server.ts';
import { serveHttp } from '../../src/transports/http-server.ts';
import { httpTransport } from '../../src/transports/http-client.ts';
import { runtime } from '../../src/transports/runtime.ts';
import { fakeHttp, initialize, wait } from '../helpers.mjs';
const auth = { authorization: 'Bearer secret' };
async function setup(attach) {
  const fake = fakeHttp(); const server = new McpServer({ name: 'guards', version: '1' });
  const listener = await serveHttp({ token: 'secret', timeout: 100 }, attach ?? (transport => server.attach(transport)), async () => fake.native);
  return { fake, server, listener, async close() { await listener.close(); await server.close(); } };
}

test('Host matching is case-insensitive but loopback origins must use HTTP and the bound port.', async () => {
  const fixture = await setup();
  try {
    const port = fixture.listener.address.port;
    assert.equal((await fixture.fake.request('POST', initialize(), { ...auth, host: `LOCALHOST:${port}` })).status, 200);
    for (const origin of [
      `https://localhost:${port}`, 'http://localhost:1', `http://evil.example:${port}`,
      `https://127.0.0.1:${port}`, 'http://127.0.0.1:1', 'http://evil.example',
    ]) {
      assert.equal((await fixture.fake.request('POST', initialize(), { ...auth, origin })).status, 403, origin);
    }
  } finally { await fixture.close(); }
});

test('A non-initialize call cannot create a fresh session even when its handler succeeds.', async () => {
  const fixture = await setup();
  // A permissive peer isolates the HTTP gate from the server's own initialization gate.
  const fake = fakeHttp(); let attached = 0;
  const listener = await serveHttp({ token: 'secret', timeout: 100 }, transport => {
    attached++;
    const peer = fixture.server.attach(transport); peer.handler = () => ({ tools: [] }); return peer;
  }, async () => fake.native);
  try {
    const response = await fake.request('POST', { jsonrpc: '2.0', id: 2, method: 'tools/list' }, auth);
    assert.equal(response.status, 400); assert.equal(response.headers.has('mcp-session-id'), false); assert.equal(attached, 0);
  } finally { await listener.close(); await fixture.close(); }
});

test('Failed initialization closes the peer and never retains its deterministic session ID.', async () => {
  const original = runtime.crypto.random;
  runtime.crypto.random = async length => new Uint8Array(length).fill(0xab);
  const fixture = await setup();
  try {
    const bad = initialize(); delete bad.params.capabilities;
    const response = await fixture.fake.request('POST', bad, auth);
    assert.equal(response.status, 400); assert.equal(response.body.error.code, -32602);
    assert.equal(response.headers.has('mcp-session-id'), false);
    assert.equal((await fixture.fake.request('POST', { jsonrpc: '2.0', id: 2, method: 'tools/list' }, {
      ...auth, 'mcp-session-id': 'ab'.repeat(32), 'mcp-protocol-version': '2025-11-25',
    })).status, 404);
  } finally { await fixture.close(); runtime.crypto.random = original; }
});

test('Listener close closes every session transport and aborts active handlers before native shutdown.', { timeout: 2000 }, async () => {
  const fake = fakeHttp(); const server = new McpServer({ name: 'close', version: '1' });
  const peers = []; let closed = 0; let signal; let finish;
  const listener = await serveHttp({ token: 'secret', timeout: 100 }, transport => {
    const peer = server.attach({ ...transport, close: async () => { closed++; await transport.close(); } }); peers.push(peer); return peer;
  }, async () => fake.native);
  let pending;
  try {
    for (let index = 0; index < 2; index++) assert.equal((await fake.request('POST', initialize(), auth)).status, 200);
    peers[0].handler = (_method, _params, context) => { signal = context.signal; return new Promise(resolve => { finish = resolve; }); };
    pending = peers[0].dispatch({ jsonrpc: '2.0', id: 8, method: 'hang' });
    await listener.close();
    assert.equal(closed, 2); assert.equal(signal.aborted, true);
    for (const peer of peers) await assert.rejects(peer.notify('ping'), /closed/);
    assert.equal(fake.closed, true);
  } finally { finish?.({}); if (pending) await wait(pending); await listener.close(); await server.close(); }
});

test('HTTP client rejects unsafe URLs before any request and never follows redirects.', async () => {
  let calls = 0;
  const requester = async (_url, options) => {
    calls++; assert.equal(options.redirect, 'manual');
    return { status: 302, headers: new Headers({ location: 'http://evil.example/mcp' }), body: null };
  };
  for (const url of ['ftp://localhost/mcp', 'file:///mcp', 'http://user:pass@localhost/mcp', 'http://localhost/mcp#fragment']) {
    assert.throws(() => httpTransport(url, undefined, 100, requester), /HTTP URL without credentials or fragment/);
  }
  assert.equal(calls, 0);
  const transport = httpTransport('http://localhost/mcp', undefined, 100, requester);
  try { await assert.rejects(transport.send(initialize()), /302/); assert.equal(calls, 1); }
  finally { await transport.close(); }
});
