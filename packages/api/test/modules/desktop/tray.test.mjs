// SPDX-License-Identifier: MIT OR Apache-2.0
import assert from 'node:assert/strict';
import test from 'node:test';
import { AlefError, Tray, menu as applicationMenu, tray } from '../../../src/index.ts';
import { on } from '../../../src/core/events.ts';
import { barrier, installRuntime, jsonFrame, liveStream } from '../../fake-runtime.mjs';

const feed = liveStream();
const reply = { id: 's31:r8', owner: 31, token: 'tray:17' };
const icon = new Uint8Array([137, 80, 78, 71, 13, 10, 26, 10]);
let failure;
let duringSubscribe;
const runtime = installRuntime({
  handler(request) {
    if (request.url === 'native://call/runtime.events.subscribe') {
      duringSubscribe?.();
      return { json: { stream: 5 } };
    }
    if (request.url === 'native://stream/5') return feed.reply;
    if (request.url === 'native://call/tray.create') return failure ?? { json: reply };
    if (request.url.startsWith('native://call/tray.')) return failure ?? { json: null };
    return undefined;
  },
});
const emit = (payload, name = 'runtime.tray.click') => feed.push(jsonFrame({ name, payload }));
const valid = { owner: 31, token: 'tray:17', button: 'left' };
const create = () => tray.create({ icon });
const bounded = (name, fn) => test(name, { timeout: 5000 }, fn);

const settle = () => barrier(feed, on, 'test.tray.marker');

bounded('create sends PNG bytes, metadata and signal, exposes only the resource id', async () => {
  const controller = new AbortController();
  const menu = [{ id: 'open', label: 'Открыть' }];
  const handle = await tray.create({ icon, tooltip: 'Трей', menu }, { signal: controller.signal });
  assert.ok(handle instanceof Tray);
  assert.equal(handle.id, reply.id);
  assert.deepEqual(Object.keys(handle), ['id']);
  const request = runtime.calls('tray.create').at(-1);
  assert.equal(request.method, 'POST');
  assert.equal(request.body, icon);
  assert.equal(request.headers['content-type'], 'application/octet-stream');
  assert.deepEqual(runtime.argsOf(request), { tooltip: 'Трей', menu });
  assert.equal(request.signal, controller.signal);
  await create();
  assert.deepEqual(runtime.argsOf(runtime.calls('tray.create').at(-1)), {});
});

bounded('all setters and destroy preserve exact arguments, bodies and signals', async () => {
  const handle = await create();
  const controller = new AbortController();
  const options = { signal: controller.signal };
  const bytes = icon.subarray(1, 4);
  await handle.setIcon(bytes, options);
  const binary = runtime.calls('tray.setIcon').at(-1);
  assert.equal(binary.body, bytes);
  assert.equal(binary.headers['content-type'], 'application/octet-stream');
  assert.deepEqual(runtime.argsOf(binary), { id: reply.id });
  assert.equal(binary.signal, controller.signal);
  const menu = [{ id: 'item', label: 'Item' }];
  await handle.setTooltip('', options);
  await handle.setMenu(menu, options);
  await handle.destroy(options);
  for (const [command, args] of [
    ['tray.setTooltip', { id: reply.id, tooltip: '' }],
    ['tray.setMenu', { id: reply.id, menu }],
    ['tray.destroy', { id: reply.id }],
  ]) {
    const request = runtime.calls(command).at(-1);
    assert.equal(request.method, 'POST');
    assert.equal(request.body, JSON.stringify(args));
    assert.equal(request.headers['content-type'], 'application/json');
    assert.equal(request.headers['x-alef-args'], undefined);
    assert.equal(request.signal, controller.signal);
  }
});

bounded('subscription catches opening-race events and filters malformed or foreign identities', async t => {
  const errors = t.mock.method(console, 'error', () => {});
  const handle = await create();
  const received = [];
  duringSubscribe = () => emit(valid);
  const off = await handle.on('click', payload => received.push(payload));
  duringSubscribe = undefined;
  await settle();
  assert.deepEqual(received, [{ tray: handle, button: 'left' }]);
  for (const payload of [null, 17, [], {},
    { ...valid, owner: 32 }, { ...valid, owner: '31' },
    { ...valid, token: 'tray:18' }, { ...valid, token: 17 },
    { ...valid, button: undefined }, { ...valid, button: 'unknown' },
    { ...valid, button: null }]) emit(payload);
  emit(valid, 'runtime.tray.unknown');
  emit(valid, 'runtime.tray.double-click');
  emit({ ...valid, button: 'middle' });
  emit({ ...valid, button: 'right' });
  await settle();
  assert.deepEqual(received.map(payload => payload.button), ['left', 'middle', 'right']);
  assert.ok(received.every(payload => payload.tray === handle));
  assert.equal(errors.mock.callCount(), 0);
  assert.equal(runtime.calls('runtime.events.subscribe').length, 1);
  off();
  emit(valid);
  await settle();
  assert.equal(received.length, 3);
});

bounded('double-click and right-click map only to their real native event names', async () => {
  const handle = await create();
  const received = [];
  const offs = await Promise.all(['double-click', 'right-click'].map(event =>
    handle.on(event, payload => received.push([event, payload]))));
  emit(valid);
  emit({ ...valid, button: 'left' }, 'runtime.tray.double-click');
  emit({ ...valid, button: 'right' }, 'runtime.tray.right-click');
  await settle();
  assert.deepEqual(received, [
    ['double-click', { tray: handle, button: 'left' }],
    ['right-click', { tray: handle, button: 'right' }],
  ]);
  offs.forEach(off => off());
});

bounded('menu-click maps runtime.tray.menu-click and requires owner, token and nonempty string id', async () => {
  const handle = await create();
  const received = [];
  const off = await handle.on('menu-click', payload => received.push(payload));
  for (const payload of [null, {}, { id: 'item' },
    { owner: 32, token: reply.token, id: 'item' },
    { owner: 31, token: 'foreign', id: 'item' },
    { owner: 31, token: reply.token, id: '' },
    { owner: 31, token: reply.token, id: 1 },
    { owner: 31, token: reply.token, id: 'item' }]) emit(payload, 'runtime.tray.menu-click');
  await settle();
  assert.deepEqual(received, [{ tray: handle, id: 'item' }]);
  off();
});

bounded('an item of a tray menu does not reach menu.on and an item of the application menu does not reach the tray', async () => {
  const handle = await create();
  const fromMenu = [];
  const fromTray = [];
  const offMenu = await applicationMenu.on('click', payload => fromMenu.push(payload));
  const offTray = await handle.on('menu-click', payload => fromTray.push(payload.id));
  emit({ owner: 31, token: reply.token, id: 'tray-item' }, 'runtime.tray.menu-click');
  emit({ owner: 31, id: 'app-item' }, 'runtime.menu.clicked');
  await settle();
  assert.deepEqual(fromMenu, [{ id: 'app-item' }]);
  assert.deepEqual(fromTray, ['tray-item']);
  offMenu();
  offTray();
});

bounded('separate tray instances filter tokens and destroyed instances stop every event kind', async () => {
  const handle = await create();
  const other = new Tray({ id: 's31:r9', owner: 31, token: 'tray:18' });
  let count = 0;
  let otherCount = 0;
  const offs = await Promise.all(['click', 'double-click', 'right-click', 'menu-click'].map(event =>
    handle.on(event, () => count++)));
  const otherOff = await other.on('click', () => otherCount++);
  emit({ ...valid, token: 'tray:18' });
  await settle();
  assert.equal(count, 0);
  assert.equal(otherCount, 1);
  await handle.destroy();
  for (const name of ['click', 'double-click', 'right-click']) emit(valid, `runtime.tray.${name}`);
  emit({ owner: 31, token: reply.token, id: 'item' }, 'runtime.tray.menu-click');
  const lateOff = await handle.on('click', () => count++);
  emit(valid);
  await settle();
  assert.equal(count, 0);
  offs.forEach(off => off());
  otherOff();
  lateOff();
});

bounded('create and all setters propagate native errors', async () => {
  const handle = await create();
  failure = { status: 403, json: { code: 'PERMISSION_DENIED', message: 'denied' } };
  try {
    for (const invoke of [create, () => handle.setIcon(icon), () => handle.setTooltip('x'), () => handle.setMenu([])]) {
      await assert.rejects(invoke(), error => error instanceof AlefError && error.code === 'PERMISSION_DENIED');
    }
  } finally {
    failure = undefined;
  }
});

bounded('failed destroy propagates and preserves event delivery until successful retry', async () => {
  const handle = await create();
  let count = 0;
  const off = await handle.on('click', () => count++);
  failure = { status: 500, json: { code: 'INTERNAL', message: 'host failed' } };
  try {
    await assert.rejects(handle.destroy(), { code: 'INTERNAL', message: 'host failed' });
  } finally {
    failure = undefined;
  }
  emit(valid);
  await settle();
  assert.equal(count, 1);
  await handle.destroy();
  emit(valid);
  await settle();
  assert.equal(count, 1);
  off();
});

bounded('already-aborted signals reject create, all mutations and listeners', async () => {
  const handle = await create();
  const controller = new AbortController();
  controller.abort();
  const options = { signal: controller.signal };
  for (const invoke of [
    () => tray.create({ icon }, options), () => handle.setIcon(icon, options),
    () => handle.setTooltip('x', options), () => handle.setMenu([], options),
    () => handle.destroy(options), () => handle.on('click', () => {}, options),
  ]) await assert.rejects(invoke(), { name: 'AbortError' });
});

bounded('aborting during listener setup or after setup removes the listener', async () => {
  const handle = await create();
  let count = 0;
  const opening = new AbortController();
  const pending = handle.on('click', () => count++, { signal: opening.signal });
  opening.abort();
  const off = await pending;
  emit(valid);
  await settle();
  assert.equal(count, 0);
  off();
  const controller = new AbortController();
  const stop = await handle.on('click', () => count++, { signal: controller.signal });
  controller.abort();
  emit(valid);
  await settle();
  assert.equal(count, 0);
  stop();
});

bounded('unknown native events reject without subscribing', async () => {
  const handle = await create();
  const before = runtime.requests.length;
  for (const event of ['', 'pressed', 'mouse-enter', 'runtime.tray.click']) {
    await assert.rejects(handle.on(event, () => {}), { code: 'INVALID_ARGUMENT' });
  }
  assert.equal(runtime.requests.length, before);
});

bounded('async disposal destroys and suppresses events when the symbol is available', async () => {
  if (typeof Symbol.asyncDispose !== 'symbol') return;
  const handle = await create();
  let count = 0;
  const off = await handle.on('click', () => count++);
  await handle[Symbol.asyncDispose]();
  assert.deepEqual(runtime.argsOf(runtime.calls('tray.destroy').at(-1)), { id: handle.id });
  emit(valid);
  await settle();
  assert.equal(count, 0);
  off();
});
