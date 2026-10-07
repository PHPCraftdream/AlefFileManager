// SPDX-License-Identifier: MIT OR Apache-2.0
import assert from 'node:assert/strict';
import test from 'node:test';
import { AlefError, notification } from '../../../src/index.ts';
import { DENIED, installRuntime } from '../../fake-runtime.mjs';

const replies = new Map();
const runtime = installRuntime({
  handler(request) {
    return replies.get(request.url.replace('native://call/', '')) ?? { json: null };
  },
});

test('show sends the options as they are and resolves with nothing', async () => {
  replies.set('notification.show', { json: null });
  const options = { title: 'Done', body: 'All saved', icon: '/icons/ok.png' };
  assert.equal(await notification.show(options), null);
  assert.equal(await notification.show({ title: 'Only a title' }), null);
  const calls = runtime.calls('notification.show').map(call => runtime.argsOf(call));
  assert.deepEqual(calls, [options, { title: 'Only a title' }]);
});

test('an unavailable desktop, a refused text and a denied icon reject with the runtime code', async () => {
  for (const [status, code] of [[501, 'NOT_AVAILABLE'], [400, 'INVALID_ARGUMENT'], [403, 'PERMISSION_DENIED']]) {
    replies.set('notification.show', { status, json: status === 403 ? DENIED : { code, message: `notification: ${code}` } });
    await assert.rejects(notification.show({ title: 't' }), error => error instanceof AlefError && error.code === code);
  }
});

test('the signal reaches fetch', async () => {
  const controller = new AbortController();
  controller.abort();
  await assert.rejects(notification.show({ title: 't' }, { signal: controller.signal }), { name: 'AbortError' });
});
