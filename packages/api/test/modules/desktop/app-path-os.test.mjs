// SPDX-License-Identifier: MIT OR Apache-2.0
import assert from 'node:assert/strict';
import test from 'node:test';
import { AlefError, app, os, path } from '../../../src/index.ts';
import { DENIED, installRuntime, jsonFrame, liveStream } from '../../fake-runtime.mjs';

const feed = liveStream();
const replies = new Map();
const runtime = installRuntime({
  handler(request) {
    if (request.url === 'native://call/runtime.events.subscribe') return { json: { stream: 5 } };
    if (request.url === 'native://stream/5') return feed.reply;
    const command = request.url.replace('native://call/', '');
    return replies.get(command) ?? { json: null };
  },
});

/** Calls `fn` and returns the command it sent with its arguments. */
async function sent(command, fn, reply = { json: null }) {
  replies.set(command, reply);
  const before = runtime.calls(command).length;
  const result = await fn();
  const calls = runtime.calls(command);
  assert.equal(calls.length, before + 1, `${command} was called once`);
  return { result, args: runtime.argsOf(calls.at(-1)) };
}

test('app commands carry their arguments and unwrap their replies', async () => {
  const info = { id: 'org.example', name: 'Example', version: '1.0.0', runtimeVersion: '0.1.0' };
  assert.deepEqual((await sent('app.info', () => app.info(), { json: info })).result, info);
  assert.equal((await sent('app.info', () => app.info())).args, null, 'no arguments travel as null');

  assert.deepEqual((await sent('app.quit', () => app.quit())).args, {}, 'no code means no field');
  assert.deepEqual((await sent('app.quit', () => app.quit(7))).args, { code: 7 });
  assert.deepEqual((await sent('app.quit', () => app.quit(0))).args, { code: 0 }, 'zero is a code');
  assert.equal((await sent('app.relaunch', () => app.relaunch())).result, null);

  const parsed = { raw: ['--port', '1'], parsed: { port: 1 }, positional: [] };
  assert.deepEqual((await sent('app.args', () => app.args(), { json: parsed })).result, parsed);
  assert.equal((await sent('app.cwd', () => app.cwd(), { json: '/work' })).result, '/work');
});

test('env asks for one name or for every listed variable', async () => {
  const one = await sent('app.env', () => app.env('HOME'), { json: '/home/me' });
  assert.deepEqual(one.args, { name: 'HOME' });
  assert.equal(one.result, '/home/me');
  assert.equal((await sent('app.env', () => app.env('NOPE'), { json: null })).result, undefined, 'unset is undefined, not null');
  const all = await sent('app.envAll', () => app.env(), { json: { LANG: 'C' } });
  assert.equal(all.args, null);
  assert.deepEqual(all.result, { LANG: 'C' });
  replies.set('app.env', { status: 403, json: DENIED });
  await assert.rejects(app.env('SECRET'), error => error instanceof AlefError && error.code === 'PERMISSION_DENIED');
});

test('path commands are plain calls', async () => {
  for (const name of ['appData', 'appConfig', 'appCache', 'temp', 'home', 'documents', 'downloads', 'desktop', 'executable']) {
    const { result, args } = await sent(`path.${name}`, () => path[name](), { json: `/${name}` });
    assert.equal(result, `/${name}`);
    assert.equal(args, null);
  }
  assert.deepEqual((await sent('path.join', () => path.join('a', 'b', '..', 'c'), { json: 'a/c' })).args, { parts: ['a', 'b', '..', 'c'] });
  assert.deepEqual((await sent('path.join', () => path.join())).args, { parts: [] });
  assert.deepEqual((await sent('path.normalize', () => path.normalize('x/./y'), { json: 'x/y' })).args, { path: 'x/./y' });
  assert.deepEqual((await sent('path.dirname', () => path.dirname('x/y'), { json: 'x' })).args, { path: 'x/y' });
  assert.deepEqual((await sent('path.basename', () => path.basename('x/y'), { json: 'y' })).args, { path: 'x/y' });
});

test('os.info and os.theme read replies; theme-changed delivers the theme itself', async () => {
  const info = { platform: 'windows', arch: 'x86_64', version: '10.0.1', locale: 'en-US', hostname: 'box' };
  assert.deepEqual((await sent('os.info', () => os.info(), { json: info })).result, info);
  assert.equal((await sent('os.theme', () => os.theme(), { json: 'dark' })).result, 'dark');

  const seen = [];
  const off = await os.on('theme-changed', theme => seen.push(theme));
  feed.push(jsonFrame({ name: 'os.theme-changed', payload: { theme: 'dark' } }));
  feed.push(jsonFrame({ name: 'os.other', payload: { theme: 'light' } }));
  feed.push(jsonFrame({ name: 'os.theme-changed', payload: { theme: 'light' } }));
  await new Promise(resolve => setTimeout(resolve, 20));
  assert.deepEqual(seen, ['dark', 'light'], 'only theme-changed, as the bare theme');
  off();
  feed.push(jsonFrame({ name: 'os.theme-changed', payload: { theme: 'dark' } }));
  await new Promise(resolve => setTimeout(resolve, 20));
  assert.deepEqual(seen, ['dark', 'light'], 'nothing after unsubscribe');
});

test('an aborted signal cancels the call', async () => {
  const controller = new AbortController();
  controller.abort();
  await assert.rejects(app.info({ signal: controller.signal }), { name: 'AbortError' });
});
