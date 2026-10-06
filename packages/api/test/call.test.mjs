// SPDX-License-Identifier: MIT OR Apache-2.0
import assert from 'node:assert/strict';
import test from 'node:test';
import { AlefError, call } from '../src/index.ts';
import { DENIED, installRuntime } from './fake-runtime.mjs';

let reply = { json: {} };
const runtime = installRuntime({ handler: () => reply });
const last = name => runtime.calls(name).at(-1);

test('a JSON call sends the arguments as JSON with the session token', async () => {
  reply = { json: { greeting: 'hi' } };
  const result = await call('app.hello', { name: 'мир', n: [1, 2] });
  assert.deepEqual(result, { greeting: 'hi' });
  const request = last('app.hello');
  assert.equal(request.method, 'POST');
  assert.equal(request.headers.authorization, 'Bearer tok');
  assert.equal(request.headers['content-type'], 'application/json');
  assert.equal(request.body, '{"name":"мир","n":[1,2]}');
});

test('a call without arguments sends null', async () => {
  reply = { json: null };
  assert.equal(await call('app.ping'), null);
  assert.equal(last('app.ping').body, 'null');
});

test('a binary body travels as octets and the arguments in x-alef-args', async () => {
  const body = new Uint8Array([0, 1, 2, 250, 255]);
  reply = { bytes: new Uint8Array([9, 8, 7]) };
  const result = await call('fs.write', { path: 'a b/ключ' }, { body });
  const request = last('fs.write');
  assert.equal(request.headers['content-type'], 'application/octet-stream');
  assert.equal(request.body, body, 'the very same bytes are handed to fetch');
  assert.deepEqual(runtime.argsOf(request), { path: 'a b/ключ' });
  assert.ok(result instanceof Uint8Array, 'a bytes reply is a Uint8Array');
  assert.deepEqual([...result], [9, 8, 7]);
});

test('the command name is encoded into one path segment', async () => {
  reply = { json: {} };
  await call('a/b ../c');
  assert.ok(runtime.requests.some(request => request.url === 'native://call/a%2Fb%20..%2Fc'));
});

test('an error response becomes an AlefError with the runtime code, status and details', async () => {
  reply = { status: 403, json: { ...DENIED, details: { scope: 'fs' } } };
  const error = await call('fs.read', {}).then(() => assert.fail('must reject'), reason => reason);
  assert.ok(error instanceof AlefError);
  assert.equal(error.name, 'AlefError');
  assert.equal(error.code, 'PERMISSION_DENIED');
  assert.equal(error.status, 403);
  assert.equal(error.message, 'permission denied');
  assert.deepEqual(error.details, { scope: 'fs' });
});

test('a failure that is not the runtime error format is a TRANSPORT error', async () => {
  for (const response of [{ status: 500, text: 'boom' }, { status: 502, json: { unexpected: true } }]) {
    reply = response;
    const error = await call('app.hello').then(() => assert.fail('must reject'), reason => reason);
    assert.ok(error instanceof AlefError);
    assert.equal(error.code, 'TRANSPORT');
    assert.equal(error.status, response.status);
  }
});

test('a network failure is a TRANSPORT error; an abort stays an AbortError', async () => {
  const real = globalThis.fetch;
  try {
    globalThis.fetch = async (url, init) => {
      if (String(url).endsWith('/app.down')) throw new TypeError('connection reset');
      return real(url, init);
    };
    const down = await call('app.down').then(() => assert.fail('must reject'), reason => reason);
    assert.ok(down instanceof AlefError);
    assert.equal(down.code, 'TRANSPORT');
    assert.equal(down.message, 'connection reset');

    const controller = new AbortController();
    controller.abort();
    reply = { json: {} };
    const aborted = await call('app.hello', null, { signal: controller.signal }).then(() => assert.fail('must reject'), reason => reason);
    assert.equal(aborted.name, 'AbortError');
    assert.ok(!(aborted instanceof AlefError), 'the standard abort error is not rewrapped');
  } finally {
    globalThis.fetch = real;
  }
});

test('the abort signal reaches fetch so the runtime can cancel the command', async () => {
  const controller = new AbortController();
  reply = { json: {} };
  await call('app.cancellable', null, { signal: controller.signal });
  assert.equal(last('app.cancellable').signal, controller.signal);
});
