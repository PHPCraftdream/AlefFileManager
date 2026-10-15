// SPDX-License-Identifier: MIT OR Apache-2.0
// The page of the File Manager talks to the framework through frontend/src/native/api.ts: the language in the
// store, the folder through fs. The runtime is a fake; the page imports @alef-runtime/api as the bundler does.
import assert from 'node:assert/strict';
import { readFileSync, readdirSync } from 'node:fs';
import { register } from 'node:module';
import { dirname, resolve } from 'node:path';
import test from 'node:test';
import { fileURLToPath, pathToFileURL } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const apiDir = resolve(here, '../../packages/api');
// Every subpath resolves to the source named by the "alef-sources" field, exactly what the bundler alias does.
const apiEntries = Object.entries(JSON.parse(readFileSync(resolve(apiDir, 'package.json'), 'utf8'))['alef-sources'])
  .map(([subpath, source]) => [
    subpath === '.' ? '@alef-runtime/api' : `@alef-runtime/api${subpath.slice(1)}`,
    pathToFileURL(resolve(apiDir, source)).href,
  ]);
register(`data:text/javascript,${encodeURIComponent(`export async function resolve(specifier, context, next) {
  const mapped = new Map(${JSON.stringify(apiEntries)}).get(specifier);
  if (mapped) return { url: mapped, shortCircuit: true };
  return next(specifier, context);
}`)}`);

const { installRuntime } = await import('../../packages/api/test/fake-runtime.mjs');
const { AlefError } = await import('../../packages/api/src/index.ts');
const { nativeApi } = await import('../src/native/api.ts');

const replies = new Map();
const runtime = installRuntime({
  handler(request) {
    const command = request.url.replace('native://call/', '');
    const reply = replies.get(command);
    return typeof reply === 'function' ? reply(runtime.argsOf(request)) : reply ?? { json: null };
  },
});

const order = [];
const track = name => { order.push(name); };

test('the language is read from the store; a missing or unknown one is Russian', async () => {
  replies.set('store.get', { json: { value: 'he' } });
  assert.deepEqual(await nativeApi.preferences(), { language: 'he' });
  assert.deepEqual(runtime.argsOf(runtime.calls('store.get').at(-1)), { key: 'language' });
  replies.set('store.get', { json: {} });
  assert.deepEqual(await nativeApi.preferences(), { language: 'ru' }, 'nothing stored');
  replies.set('store.get', { json: { value: 'klingon' } });
  assert.deepEqual(await nativeApi.preferences(), { language: 'ru' }, 'an unknown language');
  replies.set('store.get', { json: { value: 42 } });
  assert.deepEqual(await nativeApi.preferences(), { language: 'ru' }, 'not even text');
});

test('a language is stored and flushed to the disk, an unknown one is refused before anything is sent', async () => {
  replies.set('store.set', () => { track('set'); return { json: null }; });
  replies.set('store.flush', () => { track('flush'); return { json: null }; });
  order.length = 0;
  assert.deepEqual(await nativeApi.setPreferences('en'), { language: 'en' });
  assert.deepEqual(order, ['set', 'flush'], 'written, then flushed');
  assert.deepEqual(runtime.argsOf(runtime.calls('store.set').at(-1)), { key: 'language', value: 'en' });
  const calls = runtime.calls('store.set').length;
  await assert.rejects(nativeApi.setPreferences('invalid'), error => error instanceof AlefError && error.code === 'INVALID_ARGUMENT');
  assert.equal(runtime.calls('store.set').length, calls, 'nothing was sent');
});

test('the identity of the application is what app.info says', async () => {
  const info = { id: 'org.alef.filemanager', name: 'Alef File Manager', version: '0.1.0', runtimeVersion: '9.9.9' };
  replies.set('app.info', { json: info });
  assert.deepEqual(await nativeApi.info(), info);
});

const entry = (name, kind, size = 0) => ({ name, path: `/home/me/docs/${name}`, kind, size });
replies.set('path.home', { json: '/home/me' });
replies.set('path.normalize', args => ({ json: args.path.replace(/\/+$/, '') }));
replies.set('path.dirname', args => ({ json: args.path.replace(/\/[^/]*$/, '') }));
replies.set('fs.readDir', {
  json: [entry('b.txt', 'file', 6), entry('Z-folder', 'dir'), entry('a.txt', 'file', 3), entry('link', 'symlink'), entry('A-folder', 'dir')],
});

test('a folder is listed with folders first, each in order, and the parent is the folder above', async () => {
  replies.set('app.args', { json: { raw: [], parsed: { root: '/home/me' }, positional: [] } });
  const listing = await nativeApi.listDirectory('/home/me/docs');
  assert.equal(listing.root, '/home/me');
  assert.equal(listing.path, '/home/me/docs');
  assert.equal(listing.parent, '/home/me');
  assert.deepEqual(listing.entries.map(item => item.name), ['A-folder', 'Z-folder', 'a.txt', 'b.txt', 'link']);
  const [folder, , file, , link] = listing.entries;
  assert.deepEqual([folder.is_dir, folder.is_file, folder.is_symlink], [true, false, false]);
  assert.deepEqual([file.is_dir, file.is_file, file.is_symlink, file.size], [false, true, false, 3]);
  assert.deepEqual([link.is_dir, link.is_file, link.is_symlink], [false, false, true]);
  assert.deepEqual(runtime.argsOf(runtime.calls('fs.readDir').at(-1)), { path: '/home/me/docs' });
});

test('the root is where the listing starts and has no parent; --root names it, else the home folder does', async () => {
  replies.set('app.args', { json: { raw: ['--root', '/home/me/docs'], parsed: { root: '/home/me/docs' }, positional: [] } });
  const named = await nativeApi.listDirectory();
  assert.deepEqual([named.root, named.path, named.parent], ['/home/me/docs', '/home/me/docs', null]);
  assert.deepEqual(runtime.argsOf(runtime.calls('fs.readDir').at(-1)), { path: '/home/me/docs' });
  replies.set('app.args', { json: { raw: [], parsed: {}, positional: [] } });
  const home = await nativeApi.listDirectory();
  assert.deepEqual([home.root, home.path, home.parent], ['/home/me', '/home/me', null]);
  replies.set('app.args', { json: { raw: ['--root', ''], parsed: { root: '' }, positional: [] } });
  assert.equal((await nativeApi.listDirectory()).root, '/home/me', 'an empty root is no root');
});

test('a refusal of the runtime (outside the scope) reaches the page as it is', async () => {
  replies.set('app.args', { json: { raw: [], parsed: {}, positional: [] } });
  replies.set('fs.readDir', { status: 403, json: { code: 'PERMISSION_DENIED', message: 'no', details: { permission: 'fs.read' } } });
  await assert.rejects(nativeApi.listDirectory('/etc'), error => error instanceof AlefError && error.code === 'PERMISSION_DENIED');
});

test('every_api_specifier_of_the_frontend_is_an_exported_subpath_whose_aliases_target_its_source', () => {
  const specifiers = new Set();
  for (const file of readdirSync(resolve(here, '../src'), { recursive: true }).map(String)) {
    if (!/\.(ts|tsx)$/.test(file)) continue;
    for (const match of readFileSync(resolve(here, '../src', file), 'utf8').matchAll(/'@alef-runtime\/api([^']*)'/g)) {
      specifiers.add(match[1] === '' ? '.' : `.${match[1]}`);
    }
  }
  assert.ok(specifiers.size > 0, 'the frontend must import the api by its subpaths');
  const pkg = JSON.parse(readFileSync(resolve(here, '../../packages/api/package.json'), 'utf8'));
  for (const subpath of specifiers) {
    assert.ok(pkg.exports[subpath] !== undefined, `frontend imports "${subpath}", which the package does not export`);
    assert.match(String(pkg['alef-sources'][subpath] ?? ''), /^\.\/.*\.ts$/, `subpath "${subpath}" must name its monorepo source in "alef-sources"`);
  }
  // Each alias must map the specifier to the very file "alef-sources" names, not just to some api file.
  const aliasLine = /['"](@alef-runtime\/api[^'"]*)['"]\s*:\s*\[?\s*['"]([^'"]+)['"]/g;
  for (const file of ['../../tsconfig.json', '../../rsbuild.config.ts']) {
    const listed = new Map();
    for (const match of readFileSync(resolve(here, file), 'utf8').matchAll(aliasLine)) {
      const key = match[1] === '@alef-runtime/api' ? '.' : `.${match[1].slice('@alef-runtime/api'.length)}`;
      listed.set(key, match[2].replaceAll('\\', '/').replace(/^\.\//, ''));
    }
    for (const subpath of specifiers) {
      assert.equal(listed.get(subpath), `packages/api/${pkg['alef-sources'][subpath].slice(2)}`, `"${subpath}" must map to its source in ${file}`);
    }
  }
});
