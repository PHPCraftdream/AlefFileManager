// SPDX-License-Identifier: MIT OR Apache-2.0
import assert from 'node:assert/strict';
import test from 'node:test';
import { AlefError, AppWindow, screen, window } from '../../src/index.ts';
import { installRuntime, jsonFrame, liveStream } from '../fake-runtime.mjs';

const feed = liveStream();
const replies = new Map();
const runtime = installRuntime({
  handler(request) {
    if (request.url === 'native://call/runtime.events.subscribe') return { json: { stream: 5 } };
    if (request.url === 'native://stream/5') return feed.reply;
    return replies.get(request.url.replace('native://call/', '')) ?? { json: null };
  },
});

const emit = (name, payload) => feed.push(jsonFrame({ name, payload }));
const settle = () => new Promise(resolve => setTimeout(resolve, 20));
const lastArgs = command => runtime.argsOf(runtime.calls(command).at(-1));
const infoOf = label => ({ label, revision: 1, title: label });

test('current() reads the state of this document\'s window and asks without a label of its own', async () => {
  replies.set('window.state', { json: infoOf('main') });
  const main = await window.current();
  assert.ok(main instanceof AppWindow);
  assert.equal(main.label, 'main');
  assert.equal(lastArgs('window.state'), null, 'the window of the caller is the default');
  assert.deepEqual(await main.state(), infoOf('main'));
  assert.deepEqual(lastArgs('window.state'), { label: 'main' });
});

test('every operation is a window.<op> command with its fields and the label', async () => {
  const win = new AppWindow('tool');
  const cases = [
    [() => win.setTitle('Hi'), 'setTitle', { title: 'Hi' }],
    [() => win.setSize('70%work', 500), 'setSize', { width: '70%work', height: 500 }],
    [() => win.setPosition(10, '5%screen'), 'setPosition', { x: 10, y: '5%screen' }],
    [() => win.center(), 'center', {}],
    [() => win.minimize(), 'minimize', {}],
    [() => win.maximize(), 'maximize', {}],
    [() => win.restore(), 'restore', {}],
    [() => win.toggleMaximize(), 'toggleMaximize', {}],
    [() => win.setFullscreen(true), 'setFullscreen', { enabled: true }],
    [() => win.setAlwaysOnTop(false), 'setAlwaysOnTop', { enabled: false }],
    [() => win.setResizable(false), 'setResizable', { enabled: false }],
    [() => win.setDecorations(true), 'setDecorations', { enabled: true }],
    [() => win.setMinSize(300), 'setMinSize', { width: 300 }],
    [() => win.setMinSize(null, 200), 'setMinSize', { width: null, height: 200 }],
    [() => win.setMaxSize(undefined, '90%work'), 'setMaxSize', { height: '90%work' }],
    [() => win.show(), 'show', {}],
    [() => win.hide(), 'hide', {}],
    [() => win.focus(), 'focus', {}],
    [() => win.close(), 'close', {}],
    [() => win.destroy(), 'destroy', {}],
    [() => win.startDrag(), 'startDrag', {}],
    [() => win.startResize('southEast'), 'startResize', { edge: 'southEast' }],
    [() => win.setZoom(1.5), 'setZoom', { factor: 1.5 }],
  ];
  for (const [run, op, fields] of cases) {
    await run();
    assert.deepEqual(lastArgs(`window.${op}`), { ...fields, label: 'tool' }, op);
  }
});

test('create sends the definition and answers with a window that is not the caller\'s own', async () => {
  const definition = { label: 'second', url: '/second.html', width: '60%work', height: 400, position: 'center' };
  replies.set('window.create', { json: infoOf('second') });
  const created = await window.create(definition);
  assert.equal(created.label, 'second');
  assert.deepEqual(lastArgs('window.create'), definition);
  await assert.rejects(
    created.on('close-requested', () => {}),
    error => error instanceof AlefError && error.code === 'INVALID_ARGUMENT',
    'only the document of a window answers its close requests',
  );
  replies.set('window.create', { status: 403, json: { code: 'PERMISSION_DENIED', message: 'permission denied' } });
  await assert.rejects(window.create(definition), { code: 'PERMISSION_DENIED' });
});

test('all() lists the windows and knows which one is this document\'s', async () => {
  replies.set('window.all', { json: [infoOf('main'), infoOf('second')] });
  replies.set('window.state', { json: infoOf('second') });
  const windows = await window.all();
  assert.deepEqual(windows.map(w => w.label), ['main', 'second']);
  const unlisten = await windows[1].on('close-requested', () => {});
  unlisten();
  await assert.rejects(windows[0].on('close-requested', () => {}), { code: 'INVALID_ARGUMENT' });
});

test('events reach only the handlers of their own window and stop after unlisten', async () => {
  const win = new AppWindow('main');
  const seen = [];
  const stops = await Promise.all(
    ['moved', 'resized', 'focus', 'blur'].map(name => win.on(name, payload => seen.push([name, payload]))),
  );
  emit('window.moved', { label: 'main', x: 1, y: 2 });
  emit('window.moved', { label: 'other', x: 9, y: 9 });
  emit('window.resized', { label: 'main', width: 3, height: 4, scaleFactor: 1 });
  emit('window.focus', { label: 'main' });
  emit('window.blur', { label: 'other' });
  emit('window.blur', { label: 'main' });
  await settle();
  assert.deepEqual(seen, [
    ['moved', { label: 'main', x: 1, y: 2 }],
    ['resized', { label: 'main', width: 3, height: 4, scaleFactor: 1 }],
    ['focus', { label: 'main' }],
    ['blur', { label: 'main' }],
  ]);
  for (const stop of stops) stop();
  emit('window.moved', { label: 'main', x: 5, y: 5 });
  await settle();
  assert.equal(seen.length, 4, 'nothing after unlisten');
});

test('close-requested: the handler decides, the answer follows, interception ends with the last handler', async () => {
  const win = new AppWindow('main', true);
  const before = runtime.calls('window.closeIntercept').length;
  const keep = [];
  const stopFirst = await win.on('close-requested', event => {
    keep.push(event.label);
    event.preventDefault();
  });
  assert.equal(runtime.calls('window.closeIntercept').length, before + 1);
  assert.deepEqual(lastArgs('window.closeIntercept'), { enabled: true }, 'no label: the caller\'s own window');

  emit('window.close-requested', { label: 'main', id: 7 });
  await settle();
  assert.deepEqual(keep, ['main']);
  assert.deepEqual(lastArgs('window.closeAnswer'), { id: 7, prevent: true });

  const order = [];
  const stopSecond = await win.on('close-requested', async () => {
    await new Promise(resolve => setTimeout(resolve, 5));
    order.push('second');
  });
  assert.equal(runtime.calls('window.closeIntercept').length, before + 1, 'one interception for all handlers');
  emit('window.close-requested', { label: 'main', id: 8 });
  await settle();
  assert.deepEqual(order, ['second'], 'an asynchronous handler is awaited before the answer');
  assert.deepEqual(lastArgs('window.closeAnswer'), { id: 8, prevent: true }, 'one handler is enough to keep the window');

  stopFirst();
  emit('window.close-requested', { label: 'main', id: 9 });
  await settle();
  assert.deepEqual(lastArgs('window.closeAnswer'), { id: 9, prevent: false }, 'the remaining handler allows it');

  stopSecond();
  stopSecond();
  await settle();
  assert.deepEqual(lastArgs('window.closeIntercept'), { enabled: false });
  assert.equal(runtime.calls('window.closeIntercept').length, before + 2, 'stopping twice lifts it once');
});

test('a throwing close handler does not stop the answer', async () => {
  const win = new AppWindow('main', true);
  const quiet = console.error;
  console.error = () => {};
  try {
    const stop = await win.on('close-requested', () => {
      throw new Error('boom');
    });
    emit('window.close-requested', { label: 'main', id: 21 });
    await settle();
    assert.deepEqual(lastArgs('window.closeAnswer'), { id: 21, prevent: false });
    stop();
    await settle();
  } finally {
    console.error = quiet;
  }
});

test('a refused interception rejects the subscription and leaves nothing behind', async () => {
  const win = new AppWindow('main', true);
  replies.set('window.closeIntercept', { status: 500, json: { code: 'INTERNAL', message: 'no session' } });
  await assert.rejects(win.on('close-requested', () => {}), { code: 'INTERNAL' });
  replies.set('window.closeIntercept', { json: null });
  const stop = await win.on('close-requested', () => {});
  stop();
  await settle();
});

test('screen asks for the displays and the cursor', async () => {
  const monitors = [{ name: 'a', bounds: { x: 0, y: 0, width: 1, height: 1 }, workArea: { x: 0, y: 0, width: 1, height: 1 }, scaleFactor: 1, primary: true }];
  replies.set('screen.monitors', { json: monitors });
  replies.set('screen.cursorPosition', { json: { x: 3, y: 4 } });
  assert.deepEqual(await screen.monitors(), monitors);
  assert.equal(lastArgs('screen.monitors'), null);
  assert.deepEqual(await screen.cursorPosition(), { x: 3, y: 4 });
});
