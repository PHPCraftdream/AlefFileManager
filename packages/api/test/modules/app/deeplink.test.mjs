// SPDX-License-Identifier: MIT OR Apache-2.0
import assert from 'node:assert/strict';
import test from 'node:test';
import { AlefError, app } from '../../../src/index.ts';
import { installRuntime } from '../../fake-runtime.mjs';

let failure;
const runtime = installRuntime({ handler() {
  return failure ? { status: 501, json: { code: failure, message: 'registration refused' } } : { json: null };
} });

test('deep_link_commands_are_argument_free_and_forward_options', async () => {
  assert.equal(Object.hasOwn(app, 'deepLinks'), false);
  const controller = new AbortController();
  for (const method of ['registerDeepLinks', 'unregisterDeepLinks']) {
    const result = app[method]({ signal: controller.signal });
    assert.equal(typeof result.then, 'function');
    await result;
    const request = runtime.calls(`app.${method}`).at(-1);
    assert.equal(runtime.argsOf(request), null);
    assert.equal(request.signal, controller.signal);
  }
});

test('deep_link_errors_and_aborts_propagate', async () => {
  for (const code of ['PERMISSION_DENIED', 'NOT_AVAILABLE', 'INVALID_ARGUMENT']) {
    failure = code;
    for (const method of ['registerDeepLinks', 'unregisterDeepLinks']) {
      await assert.rejects(app[method](), error => error instanceof AlefError && error.code === code);
    }
  }
  failure = undefined;
  const controller = new AbortController();
  controller.abort();
  for (const method of ['registerDeepLinks', 'unregisterDeepLinks']) {
    await assert.rejects(app[method]({ signal: controller.signal }), { name: 'AbortError' });
  }
});
