// SPDX-License-Identifier: MIT OR Apache-2.0
import assert from 'node:assert/strict';
import test from 'node:test';
import { API } from 'typescript/unstable/sync';
import { fileURLToPath } from 'node:url';
import { AlefError, Shortcut, shortcut } from '../../../src/index.ts';
import { installRuntime, jsonFrame, liveStream, quiet } from '../../fake-runtime.mjs';

const feed = liveStream();
let reply = { id: 's31:r8', owner: 31, token: 17 };
let failure;
let duringSubscribe;
const runtime = installRuntime({
  handler(request) {
    if (request.url === 'native://call/runtime.events.subscribe') {
      duringSubscribe?.();
      return { json: { stream: 5 } };
    }
    if (request.url === 'native://stream/5') return feed.reply;
    if (request.url === 'native://call/shortcut.register') return failure ?? { json: reply };
    if (request.url === 'native://call/shortcut.unregister') return failure ?? { json: null };
    return undefined;
  },
});
const emit = (payload, name = 'runtime.shortcut.pressed') => feed.push(jsonFrame({ name, payload }));

test('register returns a Shortcut and sends only the accelerator with its signal', async () => {
  const controller = new AbortController();
  const handle = await shortcut.register('CommandOrControl+Shift+K', { signal: controller.signal });
  assert.ok(handle instanceof Shortcut);
  assert.equal(handle.id, 's31:r8');
  assert.deepEqual(Reflect.ownKeys(handle), ['id']);
  assert.equal('owner' in handle, false);
  assert.equal('token' in handle, false);
  assert.equal(handle.owner, undefined);
  assert.equal(handle.token, undefined);
  const request = runtime.calls('shortcut.register').at(-1);
  assert.equal(request.method, 'POST');
  assert.equal(request.body, JSON.stringify({ accelerator: 'CommandOrControl+Shift+K' }));
  assert.equal(request.headers['content-type'], 'application/json');
  assert.equal(request.headers['x-alef-args'], undefined);
  assert.equal(request.signal, controller.signal);
});

test('numeric stand-in uses the shared subscription and handle-based unregister', async () => {
  reply = { id: 's31:r9', owner: 31, token: Number.MAX_SAFE_INTEGER };
  const handle = await shortcut.register('Ctrl+K');
  let count = 0;
  const before = runtime.calls('runtime.events.subscribe').length;
  duringSubscribe = () => emit({ owner: 31, token: Number.MAX_SAFE_INTEGER });
  const off = await handle.on('pressed', () => count++);
  duringSubscribe = undefined;
  await quiet(runtime);
  assert.equal(count, 1, 'numeric identity installs the listener before the stream opens');
  count = 0;
  assert.equal(typeof off, 'function');
  assert.equal(runtime.calls('runtime.events.subscribe').length, before + 1);
  emit({ owner: 31, token: null });
  emit({ owner: 31, token: 17 });
  emit({ owner: 32, token: Number.MAX_SAFE_INTEGER });
  await quiet(runtime);
  assert.equal(count, 0, 'stand-in is inert when the host emits no matching event');
  off();
  await handle.unregister();
  assert.deepEqual(runtime.argsOf(runtime.calls('shortcut.unregister').at(-1)), { id: 's31:r9' });
});

test('public TypeScript surface hides the DTO and disallows construction', () => {
  const file = fileURLToPath(new URL('./shortcut.consumer.ts', import.meta.url));
  const source = `
    import { Shortcut, shortcut } from '../../../src/desktop/shortcut.ts';
    async function consume() {
      const handle: Shortcut = await shortcut.register('Ctrl+K');
      const id: string = handle.id;
      await handle.on('pressed', () => {});
      await handle.unregister();
      // @ts-expect-error owner is not public
      handle.owner;
      // @ts-expect-error token is not public
      handle.token;
      // @ts-expect-error wire DTO is not a public constructor argument
      new Shortcut({ id, owner: 31, token: 17 });
      // @ts-expect-error no public constructor at all
      new Shortcut();
      // @ts-expect-error no public wire factory
      Shortcut.create({ id, owner: 31, token: 17 });
    }
    void consume;
  `;
  const config = fileURLToPath(new URL('./shortcut.consumer.tsconfig.json', import.meta.url));
  const virtual = new Map([
    [file.replaceAll('\\', '/'), source],
    [config.replaceAll('\\', '/'), JSON.stringify({
      compilerOptions: {
        target: 'ES2022', module: 'ESNext', moduleResolution: 'Bundler',
        strict: true, noEmit: true, skipLibCheck: true, allowImportingTsExtensions: true,
      },
      files: [file],
    })],
  ]);
  const api = new API({
    cwd: process.cwd(),
    fs: {
      readFile: path => virtual.get(path.replaceAll('\\', '/')),
      fileExists: path => virtual.has(path.replaceAll('\\', '/')) || undefined,
    },
  });
  try {
    const snapshot = api.updateSnapshot({ openProjects: [config] });
    try {
      const program = snapshot.getProject(config).program;
      const diagnostics = [
        ...program.getProgramDiagnostics(), ...program.getGlobalDiagnostics(),
        ...program.getSyntacticDiagnostics(), ...program.getSemanticDiagnostics(),
      ];
      assert.deepEqual(diagnostics, []);
    } finally {
      snapshot.dispose();
    }
  } finally {
    api.close();
  }
});

test('shared subscription filters both owner and token', async t => {
  const failures = t.mock.method(console, 'error', () => {});
  reply = { id: 's31:r8', owner: 31, token: 17 };
  const handle = await shortcut.register('Ctrl+K');
  let count = 0;
  const off = await handle.on('pressed', () => count++);
  emit({ owner: 31, token: 17 });
  await quiet(runtime);
  assert.equal(count, 1);
  for (const payload of [null, 17, {}, { owner: 32, token: 17 }, { owner: 31, token: 18 }, { owner: '31', token: 17 }, { owner: 31, token: '17' }]) emit(payload);
  emit({ owner: 31, token: 17 }, 'runtime.shortcut.released');
  emit({ owner: 31, token: 17 }, 'runtime.shortcut.unknown');
  emit({ owner: 31, token: 17 });
  await quiet(runtime);
  assert.equal(count, 2);
  assert.equal(failures.mock.callCount(), 0, 'a payload of the wrong shape is skipped, not a failure of the handler');
  assert.equal(runtime.calls('runtime.events.subscribe').length, 1);
  off();
  emit({ owner: 31, token: 17 });
  await quiet(runtime);
  assert.equal(count, 2);
});

test('listener abort and successful unregister stop delivery; unregister preserves body and signal', async () => {
  const handle = await shortcut.register('Ctrl+K');
  let count = 0;
  const controller = new AbortController();
  const off = await handle.on('pressed', () => count++, { signal: controller.signal });
  controller.abort();
  emit({ owner: 31, token: 17 });
  await quiet(runtime);
  assert.equal(count, 0);
  off();
  const stop = await handle.on('pressed', () => count++);
  const unregisterController = new AbortController();
  await handle.unregister({ signal: unregisterController.signal });
  const request = runtime.calls('shortcut.unregister').at(-1);
  assert.equal(request.body, JSON.stringify({ id: 's31:r8' }));
  assert.equal(request.headers['content-type'], 'application/json');
  assert.equal(request.headers['x-alef-args'], undefined);
  assert.equal(request.signal, unregisterController.signal);
  emit({ owner: 31, token: 17 });
  await quiet(runtime);
  assert.equal(count, 0);
  stop();
});

test('register and unregister errors propagate; failed unregister keeps event delivery for retry', async () => {
  failure = { status: 403, json: { code: 'PERMISSION_DENIED', message: 'denied' } };
  try {
    await assert.rejects(shortcut.register('Ctrl+K'), error => error instanceof AlefError && error.code === 'PERMISSION_DENIED');
  } finally {
    failure = undefined;
  }
  const handle = await shortcut.register('Ctrl+K');
  let count = 0;
  const off = await handle.on('pressed', () => count++);
  failure = { status: 500, json: { code: 'INTERNAL', message: 'host failed' } };
  try {
    await assert.rejects(handle.unregister(), { code: 'INTERNAL', message: 'host failed' });
  } finally {
    failure = undefined;
  }
  emit({ owner: 31, token: 17 });
  await quiet(runtime);
  assert.equal(count, 1);
  await handle.unregister();
  off();
});

test('already-aborted signals reject registration, unregister and listeners including substitutes', async () => {
  const handle = await shortcut.register('Ctrl+K');
  const controller = new AbortController();
  controller.abort();
  const options = { signal: controller.signal };
  await assert.rejects(shortcut.register('Ctrl+K', options), { name: 'AbortError' });
  await assert.rejects(handle.unregister(options), { name: 'AbortError' });
  await assert.rejects(handle.on('pressed', () => {}, options), { name: 'AbortError' });
  reply = { id: 's31:r10', owner: 31, token: Number.MAX_SAFE_INTEGER };
  const substitute = await shortcut.register('Ctrl+L');
  await assert.rejects(substitute.on('pressed', () => {}, options), { name: 'AbortError' });
  reply = { id: 's31:r8', owner: 31, token: 17 };
});

test('unknown event names reject at runtime without opening a subscription', async () => {
  for (const token of [17, Number.MAX_SAFE_INTEGER]) {
    reply = { id: 's31:r8', owner: 31, token };
    const handle = await shortcut.register('Ctrl+K');
    const before = runtime.requests.length;
    await assert.rejects(handle.on('released', () => {}), { code: 'INVALID_ARGUMENT' });
    await assert.rejects(handle.on('', () => {}), { code: 'INVALID_ARGUMENT' });
    assert.equal(runtime.requests.length, before);
  }
});

test('async disposal unregisters where the symbol is available', async () => {
  if (typeof Symbol.asyncDispose !== 'symbol') return;
  const handle = await shortcut.register('Ctrl+K');
  await handle[Symbol.asyncDispose]();
  assert.deepEqual(runtime.argsOf(runtime.calls('shortcut.unregister').at(-1)), { id: handle.id });
});
