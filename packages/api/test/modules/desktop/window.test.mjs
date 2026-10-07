// SPDX-License-Identifier: MIT OR Apache-2.0
import assert from 'node:assert/strict';
import test from 'node:test';
import { nativeWindow } from '../../../src/index.ts';
import { installRuntime, jsonFrame, liveStream } from '../../fake-runtime.mjs';

const feed = liveStream();
let revision = 4;
const runtime = installRuntime({
  handler(request) {
    if (request.url === 'native://call/runtime.events.subscribe') return { json: { stream: 5 } };
    if (request.url === 'native://stream/5') return feed.reply;
    if (request.url === 'native://call/window.apply') {
      const { action } = JSON.parse(request.body);
      return action === 'getState' ? { json: { revision, title: 'snapshot' } } : { json: null };
    }
    return undefined;
  },
});

const emit = state => feed.push(jsonFrame({ name: 'runtime.window.state', payload: state }));
const settle = () => new Promise(resolve => setTimeout(resolve, 20));
const order = () => runtime.requests.map(request => request.url.replace('native://', ''));

test('watch subscribes before it reads the snapshot and drops stale revisions', async () => {
  const states = [];
  const unlisten = await nativeWindow.watch(state => states.push(state.revision));
  const sequence = order().filter(url => url !== 'call/runtime.hello');
  assert.deepEqual(
    sequence,
    ['call/runtime.events.subscribe', 'stream/5', 'call/window.apply'],
    'the event stream is open before the snapshot is requested, so no event falls into the gap',
  );
  assert.deepEqual(states, [4], 'the snapshot is delivered');

  emit({ revision: 3 });
  emit({ revision: 4 });
  emit({ revision: 6 });
  emit({ revision: 5 });
  await settle();
  assert.deepEqual(states, [4, 6], 'only strictly newer revisions pass');

  unlisten();
  emit({ revision: 9 });
  await settle();
  assert.deepEqual(states, [4, 6], 'no delivery after unlisten');
});

test('window actions are window.apply commands with their fields', async () => {
  const before = runtime.calls('window.apply').length;
  await nativeWindow.minimize();
  await nativeWindow.setDecorations(false);
  await nativeWindow.setResizable(true);
  await nativeWindow.startResize('northWest');
  await nativeWindow.toggleMaximize();
  await nativeWindow.startDrag();
  await nativeWindow.maximize();
  await nativeWindow.restore();
  await nativeWindow.close();
  const actions = runtime.calls('window.apply').slice(before).map(request => JSON.parse(request.body));
  assert.deepEqual(actions, [
    { action: 'minimize' },
    { action: 'setDecorations', enabled: false },
    { action: 'setResizable', enabled: true },
    { action: 'startResize', edge: 'northWest' },
    { action: 'toggleMaximize' },
    { action: 'startDrag' },
    { action: 'maximize' },
    { action: 'restore' },
    { action: 'close' },
  ]);
});

test('a failing snapshot unsubscribes and rejects', async () => {
  const real = globalThis.fetch;
  const delivered = [];
  try {
    globalThis.fetch = async (url, init) => {
      if (String(url) === 'native://call/window.apply' && JSON.parse(init.body).action === 'getState') {
        return Response.json({ code: 'INTERNAL', message: 'no window' }, { status: 500 });
      }
      return real(url, init);
    };
    await assert.rejects(nativeWindow.watch(state => delivered.push(state)), { code: 'INTERNAL' });
    emit({ revision: 50 });
    await settle();
    assert.deepEqual(delivered, [], 'the handler registered by the failed watch is gone');
  } finally {
    globalThis.fetch = real;
  }
});
