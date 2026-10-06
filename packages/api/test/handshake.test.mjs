// SPDX-License-Identifier: MIT OR Apache-2.0
import assert from 'node:assert/strict';
import test from 'node:test';
import { AlefError, call, connect } from '../src/index.ts';
import { DENIED, INFO, installRuntime } from './fake-runtime.mjs';

const rejection = async (promise, code) => {
  const error = await promise.then(() => assert.fail('expected a rejection'), reason => reason);
  assert.ok(error instanceof AlefError, `AlefError expected, got ${String(error)}`);
  assert.equal(error.code, code);
  return error;
};

// The failures run first: none of them may be remembered by the later, successful handshake.
test('without a capability in the URL fragment there is no runtime', async () => {
  const runtime = installRuntime({ hash: '' });
  await rejection(connect(), 'NOT_AVAILABLE');
  assert.equal(runtime.requests.length, 0, 'no request is made without a capability');
});

test('a refused handshake is reported with the runtime error', async () => {
  installRuntime({ hash: '#capability=wrong' });
  const error = await rejection(connect(), 'PERMISSION_DENIED');
  assert.equal(error.status, 403);
  assert.equal(error.message, DENIED.message);
});

test('an unreachable runtime is a TRANSPORT error', async () => {
  installRuntime();
  globalThis.fetch = async () => { throw new TypeError('network down'); };
  const error = await rejection(connect(), 'TRANSPORT');
  assert.equal(error.message, 'network down');
});

test('a runtime that speaks another protocol is refused', async () => {
  const runtime = installRuntime();
  const real = globalThis.fetch;
  globalThis.fetch = async (url, init) => {
    const response = await real(url, init);
    const body = await response.json();
    return Response.json({ ...body, protocol: 2 });
  };
  await rejection(connect(), 'NOT_AVAILABLE');
  assert.equal(runtime.requests.length, 1);
});

test('the handshake runs once per document and the token stays private', async () => {
  const runtime = installRuntime({ handler: () => ({ json: { ok: true } }) });
  const [info] = await Promise.all([connect(), connect()]);
  await Promise.all([call('app.hello'), call('app.hello')]);
  assert.equal(runtime.calls('runtime.hello').length, 1, 'one hello for every call of the document');
  assert.deepEqual(info, INFO);
  assert.ok(!('token' in info), 'connect() does not hand the session token to application code');
  const hello = runtime.calls('runtime.hello')[0];
  assert.equal(hello.headers.authorization, 'Bearer boot', 'the hello is made with the bootstrap capability');
  for (const request of runtime.calls('app.hello')) {
    assert.equal(request.headers.authorization, 'Bearer tok', 'calls use the session token');
  }
});
