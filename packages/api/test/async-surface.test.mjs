// SPDX-License-Identifier: MIT OR Apache-2.0
// Acceptance: every export of @alef-tron/api is asynchronous (returns a Promise or an AsyncIterable).
import assert from 'node:assert/strict';
import test from 'node:test';
import * as api from '../src/index.ts';
import { endFrame, installRuntime, liveStream } from './fake-runtime.mjs';

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
  'app.requestSingleInstance': [],
  'app.on': ['second-instance', () => {}],
  ...Object.fromEntries(['appData', 'appConfig', 'appCache', 'temp', 'home', 'documents', 'downloads', 'desktop', 'executable'].map(name => [`path.${name}`, []])),
  'path.join': ['a'],
  'path.normalize': ['a'],
  'path.dirname': ['a'],
  'path.basename': ['a'],
  'os.info': [],
  'os.theme': [],
  'os.on': ['theme-changed', () => {}],
  'window.current': [],
  'window.all': [],
  'window.create': [{ label: 'probe', url: '/', width: 100, height: 100 }],
  'screen.monitors': [],
  'screen.cursorPosition': [],
  'dialog.open': [],
  'dialog.save': [],
  'dialog.message': [{ message: 'probe' }],
  'dialog.confirm': [{ message: 'probe' }],
  'shell.openExternal': ['https://example.com/'],
  'shell.openPath': ['/probe'],
  'shell.showInFolder': ['/probe'],
  'shell.trash': ['/probe'],
  'notification.show': [{ title: 'probe' }],
  'cli.exec': ['node -v'],
  'cli.spawn': ['node'],
  'clipboard.readText': [],
  'clipboard.writeText': ['probe'],
  'clipboard.readHtml': [],
  'clipboard.writeHtml': ['probe'],
  'clipboard.readImage': [],
  'clipboard.writeImage': [new Uint8Array(1)],
  'fs.readBytes': ['/probe'],
  'fs.readText': ['/probe'],
  'fs.writeBytes': ['/probe', new Uint8Array(1)],
  'fs.writeText': ['/probe', 'probe'],
  'fs.stat': ['/probe'],
  'fs.lstat': ['/probe'],
  'fs.readDir': ['/probe'],
  'fs.exists': ['/probe'],
  'fs.mkdir': ['/probe'],
  'fs.remove': ['/probe'],
  'fs.rename': ['/probe', '/probe2'],
  'fs.copy': ['/probe', '/probe2'],
  'fs.open': ['/probe'],
  'fs.readDirStream': ['/probe'],
  'fs.watch': ['/probe'],
  'sqlite.open': ['/probe'],
  'crypto.random': [4],
  'crypto.digest': ['sha-256', 'x'],
  'crypto.hmac': ['sha-256', new Uint8Array(1), 'x'],
  'crypto.hmacVerify': ['sha-256', new Uint8Array(1), 'x', new Uint8Array(32)],
  'crypto.hkdf': ['sha-256', 'x', { length: 8 }],
  'crypto.pbkdf2': ['sha-256', 'x', new Uint8Array(8), 1, 8],
  'crypto.argon2id': ['x', new Uint8Array(8)],
  'crypto.scrypt': ['x', new Uint8Array(8)],
  'crypto.seal': ['aes-256-gcm', new Uint8Array(32), 'x'],
  'crypto.open': ['aes-256-gcm', new Uint8Array(32), new Uint8Array(40)],
  'crypto.ed25519Generate': [],
  'crypto.ed25519Sign': [new Uint8Array(32), 'x'],
  'crypto.ed25519Verify': [new Uint8Array(32), 'x', new Uint8Array(64)],
  'secrets.get': ['service', 'account'],
  'secrets.getText': ['service', 'account'],
  'secrets.set': ['service', 'account', 'probe'],
  'secrets.delete': ['service', 'account'],
  'http.request': ['http://127.0.0.1/'],
  'http.download': ['http://127.0.0.1/', '/probe'],
  'http.serve': [{}],
  'socket.connect': [{ host: '127.0.0.1', port: 1 }],
  'socket.listen': [{ port: 0 }],
  'socket.udp': [{}],
  'websocket.connect': ['ws://127.0.0.1/'],
  'websocket.serve': [{}],
  'store.get': ['k'],
  'store.set': ['k', 1],
  'store.delete': ['k'],
  'store.keys': [],
  'store.flush': [],
  'store.open': ['area'],
  'fs.tempFile': [],
  'fs.tempDir': [],
};

installRuntime({
  handler(request) {
    if (request.url === 'native://call/runtime.events.subscribe') return { json: { stream: 5 } };
    if (request.url === 'native://stream/5') return liveStream().reply; // a fresh body per request
    if (request.url === 'native://call/window.all') return { json: [] };
    if (request.url === 'native://call/http.request') return { json: { status: 200, statusText: 'OK', url: 'http://127.0.0.1/', redirected: false, headers: [], stream: null } };
    if (request.url === 'native://call/http.download') return { json: { stream: 9 } };
    if (request.url === 'native://stream/9') return { chunks: [endFrame()] };
    if (request.url === 'native://call/http.serve') return { json: { server: 6, requests: 51, address: { host: '127.0.0.1', port: 5 }, secure: false } };
    if (request.url === 'native://call/socket.connect') return { json: { socket: 1, read: 11, write: 12, localAddress: { host: '127.0.0.1', port: 2 }, remoteAddress: { host: '127.0.0.1', port: 1 } } };
    if (request.url === 'native://call/socket.listen') return { json: { server: 2, accept: 21, localAddress: { host: '127.0.0.1', port: 3 } } };
    if (request.url === 'native://call/websocket.connect') return { json: { socket: 5, messages: 41, protocol: '', url: 'ws://127.0.0.1/' } };
    if (request.url === 'native://call/socket.udp') return { json: { socket: 3, messages: 31, localAddress: { host: '127.0.0.1', port: 4 } } };
    if (request.url === 'native://call/cli.exec') return { json: { code: 0, signal: null, stdout: '', stderr: '' } };
    if (request.url === 'native://call/cli.spawn') return { json: { process: 9, pid: 42, stdin: 91, stdout: 92, stderr: 93 } };
    if (/^native:\/\/stream\/(11|21|31|41|51|91|92|93)$/.test(request.url)) return { chunks: [endFrame()] };
    if (request.url === 'native://call/crypto.ed25519Generate') return { json: { privateKey: '', publicKey: '' } };
    if (request.url === 'native://call/secrets.get') return { json: null };
    if (/^native:\/\/call\/(clipboard\.read(Text|Html)|fs\.readFile)$/.test(request.url)) return { bytes: new Uint8Array(0) };
    return { json: { revision: 1 } };
  },
});

test('every function exported by @alef-tron/api is asynchronous', async () => {
  const functions = functionsOf(api);
  assert.ok(functions.length >= 60, `the walk must see the whole surface, saw ${functions.length}`);
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
  assert.deepEqual(Object.keys(api).sort(), ['AlefError', 'AppWindow', 'ChildProcess', 'FileHandle', 'HttpResponse', 'HttpServer', 'ServerRequest', 'SqliteDatabase', 'SqliteStatement', 'SqliteTransaction', 'Store', 'TcpServer', 'TcpSocket', 'UdpSocket', 'WebSocketConnection', 'WebSocketServer', 'app', 'call', 'cli', 'clipboard', 'connect', 'crypto', 'dialog', 'fs', 'http', 'nativeWindow', 'notification', 'on', 'openReadable', 'openWritable', 'os', 'path', 'screen', 'secrets', 'shell', 'socket', 'sqlite', 'store', 'websocket', 'window']);
});
