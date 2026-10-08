// SPDX-License-Identifier: MIT OR Apache-2.0
import './regression/http.mjs';
import assert from 'node:assert/strict';
import test from 'node:test';
import { McpServer, mcp } from '../src/index.ts';
import { serveHttp } from '../src/transports/http-server.ts';
import { httpTransport } from '../src/transports/http-client.ts';
import { readBody } from '../src/transports/body.ts';
import { runtime } from '../src/transports/runtime.ts';
import { encode, fakeHttp, initialize, wait } from './helpers.mjs';
runtime.crypto = { ...runtime.crypto, random: async length => globalThis.crypto.getRandomValues(new Uint8Array(length)) };
async function setup(options = {}) {
  const fake = fakeHttp(); const server = new McpServer({ name: 'http', version: '1' });
  const listener = await serveHttp({ token: 'secret', timeout: 100, ...options }, transport => server.attach(transport), async () => fake.native);
  return { fake, server, listener, async close() { await listener.close(); await server.close(); } };
}
test('HTTP dispatch timeouts abort the handler and leave the session usable without active work.', { timeout: 2000 }, async () => {
  const fixture = await setup({ timeout: 30 });
  let signal; let aborted;
  const cancellation = new Promise(resolve => { aborted = resolve; });
  fixture.server.tool('hang', { inputSchema: { type: 'object' } }, (_args, context) => {
    signal = context.signal;
    return new Promise(resolve => context.signal.addEventListener('abort', () => { aborted(); resolve({ content: [] }); }, { once: true }));
  });
  try {
    const init = await fixture.fake.request('POST', initialize(), auth);
    const headers = { ...auth, 'mcp-session-id': init.headers.get('mcp-session-id'), 'mcp-protocol-version': '2025-11-25' };
    await fixture.fake.request('POST', { jsonrpc: '2.0', method: 'notifications/initialized' }, headers);
    const call = { jsonrpc: '2.0', id: 8, method: 'tools/call', params: { name: 'hang' } };
    const reply = await fixture.fake.request('POST', call, headers);
    assert.equal(reply.body.error.code, -32603);
    assert.equal(signal.aborted, true);
    await wait(cancellation);
    const reused = await fixture.fake.request('POST', { jsonrpc: '2.0', id: 8, method: 'tools/list' }, headers);
    assert.ok(reused.body.result.tools);
  } finally { await fixture.close(); }
});

test('Body cleanup releases stream locks even when cancellation itself never settles.', { timeout: 2000 }, async () => {
  const body = new ReadableStream({ cancel: () => new Promise(() => {}) });
  await assert.rejects(wait(readBody(body, 100, 20)), /timed out/);
  assert.equal(body.locked, false);
  const controller = new AbortController(); controller.abort(new Error('Stopped.'));
  const preAborted = new ReadableStream({ cancel: () => new Promise(() => {}) });
  await assert.rejects(wait(readBody(preAborted, 100, 20, controller.signal)), /Stopped/);
  assert.equal(preAborted.locked, false);
});

test('HTTP body reads and client cancellation are bounded and cancel their native work.', { timeout: 2000 }, async () => {
  let cancelled = 0;
  const body = new ReadableStream({ cancel() { cancelled++; } });
  await assert.rejects(wait(readBody(body, 100, 20)), /timed out/);
  assert.equal(cancelled, 1);
  let signal;
  const transport = httpTransport('http://localhost/mcp', 'secret', 100, async (_url, options) => {
    signal = options.signal;
    return new Promise((_resolve, reject) => signal.addEventListener('abort', () => reject(new Error('Aborted.')), { once: true }));
  });
  transport.start(() => {});
  const pending = transport.send({ jsonrpc: '2.0', id: 9, method: 'tools/list' });
  transport.cancel(9);
  await assert.rejects(wait(pending), /aborted/i); assert.equal(signal.aborted, true);
  await transport.close();
});
const auth = { authorization: 'Bearer secret' };
test('HTTP refuses every unsafe host, origin, token, malformed body and unknown session.', { timeout: 3000 }, async () => {
  const fixture = await setup({ maxBodyBytes: 400 });
  try {
    for (const [headers, status] of [
      [{ host: '' }, 400], [{ host: 'evil.test:1234' }, 421],
      [{ origin: 'https://evil.test' }, 403], [{ origin: 'null' }, 403], [{ origin: 'http://localhost:1234/path' }, 403], [{ origin: 'http://localhost:1234, http://localhost:1234' }, 403],
      [{ authorization: '' }, 401], [{ authorization: 'Bearer wrong' }, 401],
      [{ 'mcp-session-id': 'missing' }, 404], [{ 'mcp-protocol-version': 'unknown' }, 400],
      [{ 'content-type': 'text/plain' }, 415], [{ accept: 'application/json' }, 406],
    ]) assert.equal((await fixture.fake.request('POST', initialize(), { ...auth, ...headers })).status, status);
    assert.equal((await fixture.fake.request('POST', initialize())).status, 401);
    const invalid = await fixture.fake.request('POST', '{', auth); assert.equal(invalid.status, 400); assert.equal(invalid.body.error.code, -32700);
    assert.equal((await fixture.fake.request('POST', 'x'.repeat(401), auth)).status, 413);
    assert.equal((await fixture.fake.request('GET', undefined, auth)).status, 405);
    assert.equal((await fixture.fake.request('PUT', undefined, auth)).status, 405);
    assert.equal((await fixture.fake.request('DELETE', undefined, auth)).status, 400);
    assert.equal((await fixture.fake.request('POST', initialize(), auth, '/else')).status, 404);
    assert.equal((await fixture.fake.request('POST', { jsonrpc: '2.0', id: 2, method: 'tools/list' }, auth)).status, 400);
  } finally { await fixture.close(); }
  assert.equal(fixture.fake.closed, true);
});
test('The requested allowedOrigins name takes precedence and reaches the native guard.', { timeout: 2000 }, async () => {
  const fake = fakeHttp(); const server = new McpServer({ name: 'origins', version: '1' });
  let nativeOptions;
  const listener = await serveHttp({ token: 'secret', allowedOrigins: ['https://trusted.example'], origins: ['https://legacy.example'] }, transport => server.attach(transport), async options => { nativeOptions = options; return fake.native; });
  try {
    assert.deepEqual(nativeOptions.origins, ['https://trusted.example']);
    assert.equal((await fake.request('POST', initialize(), { ...auth, origin: 'https://trusted.example' })).status, 200);
    assert.equal((await fake.request('POST', initialize(), { ...auth, origin: 'https://legacy.example' })).status, 403);
    assert.equal((await fake.request('POST', initialize(), { ...auth, 'mcp-protocol-version': '2025-03-26' })).status, 400);
  } finally { await listener.close(); await server.close(); }
});

test('HTTP negotiates sessions, enforces revision headers and deletes sessions.', { timeout: 3000 }, async () => {
  const fixture = await setup();
  try {
    for (const revision of ['2025-11-25', '2025-06-18']) {
      const response = await fixture.fake.request('POST', initialize(revision), { ...auth, origin: 'http://localhost:1234' });
      assert.equal(response.status, 200); assert.equal(response.body.result.protocolVersion, revision);
      const session = response.headers.get('mcp-session-id'); assert.ok(session);
      const headers = { ...auth, 'mcp-session-id': session, 'mcp-protocol-version': revision };
      assert.equal((await fixture.fake.request('POST', { jsonrpc: '2.0', method: 'notifications/initialized' }, headers)).status, 202);
      const list = { jsonrpc: '2.0', id: 2, method: 'tools/list' };
      assert.equal((await fixture.fake.request('POST', list, headers)).status, 200);
      assert.equal((await fixture.fake.request('POST', list, { ...headers, 'mcp-protocol-version': '2025-11-25' === revision ? '2025-06-18' : '2025-11-25' })).status, 400);
      const noVersion = { ...auth, 'mcp-session-id': session };
      assert.equal((await fixture.fake.request('POST', list, noVersion)).status, 400);
      const batch = await fixture.fake.request('POST', [list], headers);
      assert.equal(Array.isArray(batch.body), false);
      assert.equal(batch.body.error.code, -32600);
      assert.equal((await fixture.fake.request('DELETE', undefined, headers)).status, 204);
      assert.equal((await fixture.fake.request('POST', list, headers)).status, 404);
    }
  } finally { await fixture.close(); }
});
test('HTTP token generation uses the native entropy wrapper and refuses malformed entropy.', { timeout: 2000 }, async () => {
  const original = runtime.crypto.random;
  runtime.crypto.random = async () => new Uint8Array(2);
  try { await assert.rejects(setup({ token: undefined }), /invalid token length/); }
  finally { runtime.crypto.random = original; }
});

test('HTTP generates tokens, limits sessions and allows only explicit security opt-outs.', { timeout: 3000 }, async () => {
  const fixture = await setup({ token: undefined, maxSessions: 1 });
  assert.match(fixture.listener.token, /^[a-f0-9]{64}$/);
  const headers = { authorization: `Bearer ${fixture.listener.token}` };
  assert.equal((await fixture.fake.request('POST', initialize(), headers)).status, 200);
  assert.equal((await fixture.fake.request('POST', initialize(), headers)).status, 503);
  await fixture.close();
  const unchecked = await setup({ token: false, checkHost: false, checkOrigin: false });
  assert.equal((await unchecked.fake.request('POST', initialize(), { host: 'evil.test', origin: 'null' })).status, 200);
  await unchecked.close();
  await assert.rejects(setup({ host: '0.0.0.0' }), /loopback/);
});
test('The HTTP client uses the runtime wrapper, token and session headers and performs cleanup.', { timeout: 3000 }, async () => {
  const fake = fakeHttp(); const originalServe = runtime.http.serve; const originalRequest = runtime.http.request;
  const calls = [];
  runtime.http.serve = async () => fake.native;
  runtime.http.request = async (_url, options) => {
    calls.push(options);
    const response = await fake.request(options.method, options.body, Object.fromEntries(new Headers(options.headers)));
    return { status: response.status, headers: response.headers, body: response.body === undefined ? null : new ReadableStream({ start(controller) { controller.enqueue(encode(JSON.stringify(response.body))); controller.close(); } }) };
  };
  const server = mcp.server({ name: 'web', version: '1' }).tool('x', { inputSchema: { type: 'object' } }, () => ({ content: [] }));
  try {
    await server.listen({ http: { timeout: 100 } }); assert.ok(server.token); assert.equal(server.url, 'http://127.0.0.1:1234/mcp');
    await assert.rejects(wait(mcp.connect({ url: server.url, timeout: 100 })), /401/);
    const client = await wait(mcp.connect({ url: server.url, token: server.token, timeout: 100 }));
    assert.equal((await client.listTools()).tools[0].name, 'x');
    assert.deepEqual(await client.callTool('x'), { content: [] });
    await client.close(); assert.equal(calls.at(-1).method, 'DELETE');
    assert.ok(calls.at(-1).headers['mcp-session-id']);
  } finally { await server.close(); runtime.http.serve = originalServe; runtime.http.request = originalRequest; }
  assert.equal(fake.closed, true);
});
