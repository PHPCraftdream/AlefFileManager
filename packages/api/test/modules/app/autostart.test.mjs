// SPDX-License-Identifier: MIT OR Apache-2.0
import assert from 'node:assert/strict';
import test from 'node:test';
import { AlefError, app } from '../../../src/index.ts';
import { installRuntime } from '../../fake-runtime.mjs';

let enabled = false;
let failure;
const runtime = installRuntime({ handler(request) {
  if (!request.url.includes('/app.autostart.')) return { json: null };
  if (failure) return { status: 501, json: { code: failure, message: 'autostart refused' } };
  if (request.url.endsWith('.enable')) enabled = true;
  if (request.url.endsWith('.disable')) enabled = false;
  return { json: request.url.endsWith('.isEnabled') ? enabled : null };
} });

test('autostart commands are asynchronous, argument-free and forward options', async () => {
  const controller = new AbortController();
  for (const [method, expected] of [['enable', null], ['isEnabled', true], ['disable', null], ['isEnabled', false]]) {
    const result = app.autostart[method]({ signal: controller.signal });
    assert.equal(typeof result.then, 'function');
    assert.equal(await result, expected);
    const request = runtime.calls(`app.autostart.${method}`).at(-1);
    assert.equal(runtime.argsOf(request), null);
    assert.equal(request.signal, controller.signal);
  }
});

test('autostart errors and aborts propagate', async () => {
  for (const code of ['PERMISSION_DENIED', 'NOT_AVAILABLE', 'INVALID_ARGUMENT']) {
    failure = code;
    for (const method of ['enable', 'disable', 'isEnabled']) {
      await assert.rejects(app.autostart[method](), error => error instanceof AlefError && error.code === code);
    }
  }
  failure = undefined;
  const controller = new AbortController();
  controller.abort();
  await assert.rejects(app.autostart.enable({ signal: controller.signal }), { name: 'AbortError' });
});
