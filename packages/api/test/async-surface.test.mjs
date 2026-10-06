// SPDX-License-Identifier: MIT OR Apache-2.0
// Acceptance: every export of @alef-tron/api is asynchronous (returns a Promise or an AsyncIterable).
import assert from 'node:assert/strict';
import test from 'node:test';
import * as api from '../src/index.ts';
import { installRuntime, liveStream } from './fake-runtime.mjs';

const isClass = value => typeof value === 'function' && /^class\s/.test(Function.prototype.toString.call(value));
const isThenable = value => typeof value?.then === 'function';
const isAsyncIterable = value => typeof value?.[Symbol.asyncIterator] === 'function';

/** Calls `fn` and returns its result when it is asynchronous; throws when it is not. */
function expectAsync(name, fn, args) {
  const result = fn(...args);
  assert.ok(
    isThenable(result) || isAsyncIterable(result),
    `${name}() must return a Promise or an AsyncIterable, got ${result === null ? 'null' : typeof result}`,
  );
  return result;
}

/** Every function export (one level into plain objects) with a qualified name; classes are skipped. */
function functionsOf(module) {
  const found = [];
  for (const [name, value] of Object.entries(module)) {
    if (isClass(value)) continue;
    if (typeof value === 'function') found.push([name, value]);
    else if (typeof value === 'object' && value !== null) {
      for (const [key, member] of Object.entries(value)) {
        if (typeof member === 'function') found.push([`${name}.${key}`, member]);
      }
    }
  }
  return found;
}

// Arguments for each function; a new export without an entry fails the test on purpose.
const ARGS = {
  connect: [],
  call: ['app.hello'],
  openReadable: [5],
  openWritable: [5],
  on: ['probe', () => {}],
  'nativeWindow.getState': [],
  'nativeWindow.minimize': [],
  'nativeWindow.maximize': [],
  'nativeWindow.restore': [],
  'nativeWindow.toggleMaximize': [],
  'nativeWindow.close': [],
  'nativeWindow.setDecorations': [true],
  'nativeWindow.setResizable': [true],
  'nativeWindow.startDrag': [],
  'nativeWindow.startResize': ['north'],
  'nativeWindow.watch': [() => {}],
  'app.info': [],
  'app.quit': [],
  'app.relaunch': [],
  'app.args': [],
  'app.env': [],
  'app.cwd': [],
  ...Object.fromEntries(['appData', 'appConfig', 'appCache', 'temp', 'home', 'documents', 'downloads', 'desktop', 'executable'].map(name => [`path.${name}`, []])),
  'path.join': ['a'],
  'path.normalize': ['a'],
  'path.dirname': ['a'],
  'path.basename': ['a'],
  'os.info': [],
  'os.theme': [],
  'os.on': ['theme-changed', () => {}],
};

installRuntime({
  handler(request) {
    if (request.url === 'native://call/runtime.events.subscribe') return { json: { stream: 5 } };
    if (request.url === 'native://stream/5') return liveStream().reply; // a fresh body per request
    return { json: { revision: 1 } };
  },
});

test('every function exported by @alef-tron/api is asynchronous', async () => {
  const functions = functionsOf(api);
  assert.ok(functions.length >= 38, `the walk must see the whole surface, saw ${functions.length}`);
  const unlisted = functions.map(([name]) => name).filter(name => !(name in ARGS));
  assert.deepEqual(unlisted, [], 'add the new export to ARGS so its asynchrony is checked');
  for (const [name, fn] of functions) {
    const result = expectAsync(name, fn, ARGS[name]);
    const settled = isThenable(result) ? await result : result;
    if (typeof settled === 'function') settled(); // an unsubscribe function
    else if (typeof settled?.close === 'function') await settled.close();
  }
});

test('the checker rejects a synchronous export', () => {
  const surface = { fine: async () => 1, nested: { sync: () => 1 } };
  const found = functionsOf(surface);
  assert.deepEqual(found.map(([name]) => name), ['fine', 'nested.sync']);
  assert.doesNotThrow(() => expectAsync('fine', surface.fine, []));
  assert.throws(() => expectAsync('nested.sync', surface.nested.sync, []), /must return a Promise or an AsyncIterable/);
  assert.ok(isClass(api.AlefError), 'classes are recognised and skipped');
});

test('exports other than functions are only classes and nothing mutable', () => {
  for (const [name, value] of Object.entries(api)) {
    const kind = isClass(value) ? 'class' : typeof value;
    assert.ok(['class', 'function', 'object'].includes(kind), `${name} is a ${kind}`);
  }
  assert.deepEqual(Object.keys(api).sort(), ['AlefError', 'app', 'call', 'connect', 'nativeWindow', 'on', 'openReadable', 'openWritable', 'os', 'path']);
});
