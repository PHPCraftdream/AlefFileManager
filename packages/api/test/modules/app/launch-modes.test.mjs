// SPDX-License-Identifier: MIT OR Apache-2.0
import assert from 'node:assert/strict';
import test from 'node:test';
import { AlefError, app } from '../../../src/index.ts';
import { installRuntime } from '../../fake-runtime.mjs';

let reply = { json: null };
const runtime = installRuntime({ handler: () => reply });

test('launch mode calls carry exact commands, payloads and signals', { timeout: 5000 }, async () => {
  const signal = new AbortController().signal;
  for (const mode of ['undecided', 'cli', 'foreground', 'window']) {
    reply = { json: mode };
    const before = runtime.calls('app.mode').length;
    const pending = app.mode({ signal });
    assert.equal(typeof pending.then, 'function');
    assert.equal(await pending, mode);
    assert.equal(runtime.calls('app.mode').length, before + 1);
    const request = runtime.calls('app.mode').at(-1);
    assert.equal(runtime.argsOf(request), null);
    assert.equal(request.signal, signal);
  }
  reply = { json: null };
  for (const mode of ['cli', 'foreground', 'window']) {
    const before = runtime.calls('app.launch').length;
    const pending = app.launch(mode, { signal });
    assert.equal(typeof pending.then, 'function');
    await pending;
    assert.equal(runtime.calls('app.launch').length, before + 1);
    const request = runtime.calls('app.launch').at(-1);
    assert.deepEqual(runtime.argsOf(request), { mode });
    assert.equal(request.signal, signal);
  }
});

test('launch mode wrappers forward runtime errors and aborted signals', { timeout: 5000 }, async () => {
  for (const code of ['INVALID_ARGUMENT', 'ALREADY_EXISTS', 'NOT_AVAILABLE']) {
    reply = { status: 400, json: { code, message: 'runtime refused launch selection' } };
    for (const attempt of [() => app.mode(), () => app.launch('cli')]) {
      await assert.rejects(attempt(), error => error instanceof AlefError && error.code === code
        && error.message === 'runtime refused launch selection');
    }
  }
  reply = { json: null };
  const controller = new AbortController();
  controller.abort();
  await assert.rejects(app.mode({ signal: controller.signal }), { name: 'AbortError' });
  await assert.rejects(app.launch('cli', { signal: controller.signal }), { name: 'AbortError' });
});
