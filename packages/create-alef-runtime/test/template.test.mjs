// SPDX-License-Identifier: MIT OR Apache-2.0
import assert from 'node:assert/strict';
import test from 'node:test';
import { copyFile, mkdir, mkdtemp, realpath, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { pathToFileURL } from 'node:url';

const template = new URL('../templates/base/src/permissions.js', import.meta.url);

// The template imports `@alef-runtime/api` subpaths; a stub package next to a copy of the file lets the real
// source run against fakes without installing anything.
async function loadExample() {
  const root = await realpath(await mkdtemp(path.join(tmpdir(), 'create-alef-template-')));
  const stub = path.join(root, 'node_modules', '@alef-runtime', 'api');
  await mkdir(stub, { recursive: true });
  await writeFile(path.join(root, 'package.json'), '{"type":"module"}');
  await writeFile(path.join(stub, 'package.json'), '{"name":"@alef-runtime/api","type":"module","exports":{".":"./index.js","./fs":"./index.js","./http":"./index.js","./path":"./index.js"}}');
  await writeFile(path.join(stub, 'index.js'), 'export const fs = {}; export const http = {}; export const path = {};\n');
  await mkdir(path.join(root, 'src'));
  await copyFile(template, path.join(root, 'src', 'permissions.js'));
  const module = await import(pathToFileURL(path.join(root, 'src', 'permissions.js')).href);
  return { module, cleanup: () => rm(root, { recursive: true, force: true }) };
}

// Records every timer the example sets and which of them it clears.
function watchTimers() {
  const set = new Set();
  const cleared = new Set();
  const { setTimeout: realSet, clearTimeout: realClear } = globalThis;
  globalThis.setTimeout = (...args) => { const timer = realSet(...args); set.add(timer); return timer; };
  globalThis.clearTimeout = timer => { cleared.add(timer); return realClear(timer); };
  return { set, cleared, restore() { globalThis.setTimeout = realSet; globalThis.clearTimeout = realClear; } };
}

test('the_example_cancels_the_body_of_a_successful_response_and_clears_its_timer', { timeout: 30000 }, async () => {
  const { module, cleanup } = await loadExample();
  const timers = watchTimers();
  try {
    let cancelled = 0;
    const deps = {
      path: { appData: async () => '/data' },
      fs: { readDir: async () => [{ name: 'a.txt' }, { name: 'b.txt' }] },
      http: { request: async () => ({ status: 200, body: { cancel: async () => { cancelled += 1; } } }) },
    };
    const messages = await module.permissionExample(deps);
    assert.deepEqual(messages, ['App-data entries: a.txt, b.txt', 'HTTP status: 200']);
    assert.equal(cancelled, 1, 'the unread body must be cancelled exactly once');
    assert.ok(timers.set.size > 0 && [...timers.set].every(timer => timers.cleared.has(timer)), 'every timer is cleared');
  } finally { timers.restore(); await cleanup(); }
});

test('the_example_treats_denied_permissions_timeouts_and_empty_results_as_ordinary_outcomes', { timeout: 30000 }, async () => {
  const { module, cleanup } = await loadExample();
  const timers = watchTimers();
  try {
    const denied = Object.assign(new Error('no'), { code: 'PERMISSION_DENIED' });
    const timedOut = Object.assign(new Error('late'), { code: 'TIMEOUT' });
    assert.deepEqual(await module.permissionExample({
      path: { appData: async () => '/data' },
      fs: { readDir: async () => { throw denied; } },
      http: { request: async () => { throw timedOut; } },
    }), [
      'Files: Permission denied (intentional with the closed starter manifest).',
      'HTTP: Request timed out or was cancelled.',
    ]);
    assert.deepEqual(await module.permissionExample({
      path: { appData: async () => '/data' },
      fs: { readDir: async () => [] },
      http: { request: async () => ({ status: 204 }) },
    }), ['App-data directory is empty.', 'HTTP status: 204']);
    assert.ok([...timers.set].every(timer => timers.cleared.has(timer)), 'timers are cleared on every path');
  } finally { timers.restore(); await cleanup(); }
});
