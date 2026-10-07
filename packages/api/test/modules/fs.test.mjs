// SPDX-License-Identifier: MIT OR Apache-2.0
import assert from 'node:assert/strict';
import test from 'node:test';
import { AlefError, fs } from '../../src/index.ts';
import { installRuntime } from '../fake-runtime.mjs';

const replies = new Map();
const runtime = installRuntime({
  handler(request) {
    return replies.get(request.url.replace('native://call/', '')) ?? { json: null };
  },
});

async function sent(command, fn, reply = { json: null }) {
  replies.set(command, reply);
  const before = runtime.calls(command).length;
  const result = await fn();
  const calls = runtime.calls(command);
  assert.equal(calls.length, before + 1, `${command} was called once`);
  return { result, request: calls.at(-1), args: runtime.argsOf(calls.at(-1)) };
}

const encode = text => new TextEncoder().encode(text);

test('readBytes and readText send the path and give back the bytes or the text', async () => {
  const bytes = await sent('fs.readFile', () => fs.readBytes('/a.bin'), { bytes: new Uint8Array([1, 2, 3]) });
  assert.deepEqual(bytes.args, { path: '/a.bin' });
  assert.deepEqual([...bytes.result], [1, 2, 3]);

  const text = await sent('fs.readFile', () => fs.readText('/a.txt'), { bytes: encode('héllo') });
  assert.equal(text.result, 'héllo');
  const bom = await sent('fs.readFile', () => fs.readText('/a.txt'), { bytes: new Uint8Array([0xef, 0xbb, 0xbf, 0x61]) });
  assert.equal(bom.result, '﻿a', 'a byte order mark is part of the text');
  const wide = await sent('fs.readFile', () => fs.readText('/w.txt', { encoding: 'utf-16le' }), { bytes: new Uint8Array([0x61, 0x00, 0x62, 0x00]) });
  assert.equal(wide.result, 'ab');
  assert.deepEqual(wide.args, { path: '/w.txt' }, 'the encoding is for the page to apply, not for the runtime');
});

test('writeBytes and writeText send the data as the body and the flags as arguments', async () => {
  const written = await sent('fs.writeFile', () => fs.writeBytes('/a.bin', new Uint8Array([9, 8])));
  assert.deepEqual(written.args, { path: '/a.bin' }, 'flags that were not given are not sent');
  assert.deepEqual([...written.request.body], [9, 8]);
  assert.equal(written.request.headers['content-type'], 'application/octet-stream');

  const text = await sent('fs.writeFile', () => fs.writeText('/a.txt', 'héllo', { append: true, create: false }));
  assert.deepEqual(text.args, { path: '/a.txt', append: true, create: false });
  assert.deepEqual([...text.request.body], [...encode('héllo')], 'text goes as UTF-8');
  const empty = await sent('fs.writeFile', () => fs.writeText('/e.txt', ''));
  assert.equal(empty.request.body.length, 0, 'an empty file is a body of no bytes');
});

test('the commands that look carry the path and give back what the runtime found', async () => {
  const stat = { kind: 'file', size: 3, modified: 1.7e12, readonly: false };
  assert.deepEqual((await sent('fs.stat', () => fs.stat('/a'), { json: stat })).result, stat);
  assert.deepEqual((await sent('fs.lstat', () => fs.lstat('/a'), { json: { ...stat, kind: 'symlink' } })).args, { path: '/a' });
  const listing = [{ name: 'x', path: '/d/x', kind: 'dir', size: 0 }];
  const listed = await sent('fs.readDir', () => fs.readDir('/d'), { json: listing });
  assert.deepEqual(listed.args, { path: '/d' });
  assert.deepEqual(listed.result, listing);
  assert.equal((await sent('fs.exists', () => fs.exists('/a'), { json: true })).result, true);
});

test('the commands that change carry their paths and options', async () => {
  assert.deepEqual((await sent('fs.mkdir', () => fs.mkdir('/d/e', { recursive: true }))).args, { path: '/d/e', recursive: true });
  assert.deepEqual((await sent('fs.mkdir', () => fs.mkdir('/d'))).args, { path: '/d' });
  assert.deepEqual((await sent('fs.remove', () => fs.remove('/d', { recursive: true }))).args, { path: '/d', recursive: true });
  assert.deepEqual((await sent('fs.rename', () => fs.rename('/a', '/b'))).args, { from: '/a', to: '/b' });
  assert.deepEqual((await sent('fs.copy', () => fs.copy('/a', '/b'))).args, { from: '/a', to: '/b' });
  assert.equal((await sent('fs.tempFile', () => fs.tempFile(), { json: '/cache/tmp/alef-1' })).result, '/cache/tmp/alef-1');
  assert.equal((await sent('fs.tempDir', () => fs.tempDir(), { json: '/cache/tmp/alef-2' })).result, '/cache/tmp/alef-2');
});

test('a failure keeps the code of its cause and the signal reaches fetch', async () => {
  for (const code of ['NOT_FOUND', 'ALREADY_EXISTS', 'NOT_A_DIRECTORY', 'IS_A_DIRECTORY', 'DIRECTORY_NOT_EMPTY', 'PERMISSION_DENIED']) {
    replies.set('fs.remove', { status: 409, json: { code, message: 'no' } });
    await assert.rejects(fs.remove('/d'), error => error instanceof AlefError && error.code === code, code);
  }
  const controller = new AbortController();
  controller.abort();
  await assert.rejects(fs.readBytes('/a', { signal: controller.signal }), { name: 'AbortError' });
});
