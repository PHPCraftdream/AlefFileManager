// SPDX-License-Identifier: MIT OR Apache-2.0
import assert from 'node:assert/strict';
import test from 'node:test';
import { AlefError } from '../../../src/core/errors.ts';
import { log } from '../../../src/data/log.ts';
import { installRuntime } from '../../fake-runtime.mjs';

const sample = [{ time: '2026-10-20T12:34:56.123Z', level: 'info', message: 'started', fields: { service: 'api' } }];
const crashSample = [{ id: 'crash-1', time: '2026-10-20T12:34:56Z', size: 42 }];

async function withRuntime(t, handler) {
  const previousFetch = globalThis.fetch;
  const previousLocation = globalThis.location;
  const runtime = installRuntime({ handler });
  t.after(() => {
    globalThis.fetch = previousFetch;
    globalThis.location = previousLocation;
  });
  return runtime;
}

test('log writes use all four command names and serialize message and fields', async t => {
  const runtime = await withRuntime(t, () => ({ json: null }));
  for (const [level, method] of [['debug', log.debug], ['info', log.info], ['warn', log.warn], ['error', log.error]]) {
    await method('hello', { requestId: 'r1' });
    assert.deepEqual(runtime.argsOf(runtime.calls(`log.${level}`).at(-1)), { message: 'hello', fields: { requestId: 'r1' } });
  }
  await log.info('without fields');
  assert.deepEqual(runtime.argsOf(runtime.calls('log.info').at(-1)), { message: 'without fields' });
});

test('read sends arguments and signal separately and returns runtime rows', async t => {
  const runtime = await withRuntime(t, request => request.url === 'native://call/log.read' ? { json: sample } : { json: null });
  const result = await log.read();
  assert.deepEqual(result, sample);
  assert.equal(typeof result[0].time, 'string');
  assert.deepEqual(runtime.argsOf(runtime.calls('log.read').at(-1)), {});

  const controller = new AbortController();
  await log.read({ limit: 7, level: 'warn', signal: controller.signal });
  const request = runtime.calls('log.read').at(-1);
  assert.deepEqual(runtime.argsOf(request), { limit: 7, level: 'warn' });
  assert.equal(request.signal, controller.signal);
});

test('bounded reads and crash metadata pass through unchanged', async t => {
  const bounded = Array.from({ length: 10 }, (_, i) => ({ time: '2026-10-20T12:34:56Z', level: 'info', message: `row-${i}`, fields: {} }));
  const runtime = await withRuntime(t, request => {
    const command = request.url.replace('native://call/', '');
    if (command === 'log.read') return { json: bounded };
    if (command === 'log.crashes') return { json: crashSample };
    if (command === 'log.crash') return { json: 'panic details' };
    return { json: null };
  });
  assert.deepEqual(await log.read({ limit: 1000 }), bounded);
  assert.deepEqual(runtime.argsOf(runtime.calls('log.read').at(-1)), { limit: 1000 });
  assert.deepEqual(await log.crashes(), crashSample);
  assert.deepEqual(runtime.argsOf(runtime.calls('log.crashes').at(-1)), {});
  assert.equal(await log.crash('crash-1'), 'panic details');
  assert.deepEqual(runtime.argsOf(runtime.calls('log.crash').at(-1)), { id: 'crash-1' });
});

test('crash payload at 64 KiB is returned as provided and runtime rejection propagates', async t => {
  const payload = 'x'.repeat(64 * 1024);
  const runtime = await withRuntime(t, request => {
    if (request.url === 'native://call/log.crash') return { json: payload };
    if (request.url === 'native://call/log.read') return { status: 400, json: { code: 'INVALID_ARGUMENT', message: 'invalid' } };
    return { json: null };
  });
  assert.equal(await log.crash('large'), payload);
  assert.deepEqual(runtime.argsOf(runtime.calls('log.crash').at(-1)), { id: 'large' });
  await assert.rejects(log.read(), error => error instanceof AlefError && error.code === 'INVALID_ARGUMENT');
  assert.equal(runtime.calls('log.read').length, 1);
});

 test('read forwards an aborted signal to the runtime', async t => {
  const runtime = await withRuntime(t, () => ({ json: sample }));
  const controller = new AbortController();
  controller.abort();
  await assert.rejects(log.read({ signal: controller.signal }), { name: 'AbortError' });
  assert.equal(runtime.calls('log.read').at(-1).signal, controller.signal);
});
