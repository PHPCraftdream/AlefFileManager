// SPDX-License-Identifier: MIT OR Apache-2.0
import assert from 'node:assert/strict';
import test from 'node:test';
import { AlefError, app } from '../../../src/index.ts';
import { installRuntime } from '../../fake-runtime.mjs';

const refusal = { status: 501, json: { code: 'NOT_AVAILABLE', message: 'This application is not a console utility' } };
installRuntime({ handler: request => (/app\.(stdin|stdout|stderr)$/.test(request.url) ? refusal : { json: null }) });

const notAvailable = error => error instanceof AlefError && error.code === 'NOT_AVAILABLE';

test('the standard streams of an application that is no console utility fail with NOT_AVAILABLE when they are used', async () => {
  await assert.rejects(app.stdin.getReader().read(), notAvailable);
  await assert.rejects(app.stdout.getWriter().write(new Uint8Array([1])), notAvailable);
  await assert.rejects(app.stderr.getWriter().write(new Uint8Array([1])), notAvailable);
});
