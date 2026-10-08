// SPDX-License-Identifier: MIT OR Apache-2.0
import assert from 'node:assert/strict';
import test from 'node:test';
import { app } from '../../../api/src/desktop/app.ts';
import { mcp } from '../../src/index.ts';
import { consoleStreams, runtime } from '../../src/transports/runtime.ts';
import { fakeHttp, initialize, wait } from '../helpers.mjs';

const info = { name: 'console', version: '1' };

test('The console streams of the server are the standard streams of the application.', () => {
  const { input, output } = consoleStreams();
  assert.strictEqual(input, app.stdin);
  assert.strictEqual(output, app.stdout);
});

test('A stdio server is closed when its input ends, so that a console utility can leave.', { timeout: 2000 }, async () => {
  let input;
  const server = mcp.server(info);
  await server.listen({ stdio: { input: new ReadableStream({ start(controller) { input = controller; } }), output: new WritableStream() } });
  let settled = false;
  void server.closed.then(() => { settled = true; });
  await new Promise(resolve => setTimeout(resolve, 50));
  assert.equal(settled, false, 'an open input keeps the server');
  input.close();
  await wait(server.closed);
  await server.close();
});

test('A server on a transport is closed with its only connection, and an HTTP server only by close.', { timeout: 3000 }, async () => {
  const left = { send: async () => {}, start(receive, end) { this.end = end; }, close: async () => {} };
  const single = mcp.server(info);
  await single.listen({ transport: left });
  left.end();
  await wait(single.closed);

  const original = runtime.http.serve;
  const fake = fakeHttp();
  runtime.http.serve = async () => fake.native;
  runtime.crypto = { ...runtime.crypto, random: async length => globalThis.crypto.getRandomValues(new Uint8Array(length)) };
  try {
    const server = mcp.server(info);
    await server.listen({ http: { token: 'secret', timeout: 100 } });
    let settled = false;
    void server.closed.then(() => { settled = true; });
    const auth = { authorization: 'Bearer secret' };
    const session = (await fake.request('POST', initialize(), auth)).headers.get('mcp-session-id');
    await fake.request('DELETE', undefined, { ...auth, 'mcp-session-id': session, 'mcp-protocol-version': '2025-11-25' });
    await new Promise(resolve => setTimeout(resolve, 50));
    assert.equal(settled, false, 'a session that leaves does not close an HTTP server');
    await server.close();
    await wait(server.closed);
  } finally { runtime.http.serve = original; }
});
