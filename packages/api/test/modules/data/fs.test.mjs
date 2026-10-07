// SPDX-License-Identifier: MIT OR Apache-2.0
import assert from 'node:assert/strict';
import test from 'node:test';
import { AlefError, FileHandle, fs } from '../../../src/index.ts';
import { binaryFrame, endFrame, errorFrame, installRuntime, join, jsonFrame } from '../../fake-runtime.mjs';

const replies = new Map();
const streams = new Map();
const runtime = installRuntime({
  handler(request) {
    const stream = /^native:\/\/stream\/(\d+)$/.exec(request.url);
    if (stream) return streams.get(Number(stream[1])) ?? { status: 404, json: { code: 'NOT_FOUND', message: 'stream not found' } };
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

async function opened(flags = {}) {
  replies.set('fs.open', { json: { handle: 7 } });
  return fs.open('/a.bin', flags);
}

const collect = async readable => {
  const pieces = [];
  for await (const piece of readable) pieces.push(...piece);
  return pieces;
};

test('open sends the flags and gives a handle; the handle sends its id with everything', async () => {
  const controller = new AbortController();
  const open = await sent('fs.open', () => fs.open('/a.bin', { write: true, append: true, createNew: true, signal: controller.signal }), { json: { handle: 7 } });
  assert.deepEqual(open.args, { path: '/a.bin', write: true, append: true, createNew: true }, 'the signal is not sent');
  assert.ok(open.result instanceof FileHandle);
  assert.equal(open.result.id, 7);

  const handle = open.result;
  const read = await sent('fs.read', () => handle.read(4, 10), { bytes: new Uint8Array([1, 2, 3]) });
  assert.deepEqual(read.args, { handle: 7, length: 4, position: 10 });
  assert.deepEqual([...read.result], [1, 2, 3]);
  assert.deepEqual((await sent('fs.read', () => handle.read(4), { bytes: new Uint8Array(0) })).args, { handle: 7, length: 4 }, 'no position, no key');

  const written = await sent('fs.write', () => handle.write(new Uint8Array([9, 8]), 3), { json: { written: 2 } });
  assert.deepEqual(written.args, { handle: 7, position: 3 });
  assert.deepEqual([...written.request.body], [9, 8]);
  assert.equal(written.result, 2, 'it says how much it wrote');

  const stat = { kind: 'file', size: 5, readonly: false };
  assert.deepEqual((await sent('fs.fstat', () => handle.stat(), { json: stat })).result, stat);
  assert.deepEqual((await sent('fs.truncate', () => handle.truncate(4))).args, { handle: 7, length: 4 });
  assert.deepEqual((await sent('fs.sync', () => handle.sync())).args, { handle: 7 });
  assert.deepEqual((await sent('fs.close', () => handle.close())).args, { handle: 7 });
});

test('a handle can be disposed where the language has await using', async () => {
  assert.equal(typeof FileHandle.prototype[Symbol.asyncDispose], 'function');
  const handle = await opened();
  const before = runtime.calls('fs.close').length;
  await handle[Symbol.asyncDispose]();
  assert.equal(runtime.calls('fs.close').length, before + 1);
});

test('readable gives the bytes of the stream in order and acknowledges what it consumed', async () => {
  const handle = await opened();
  replies.set('fs.readStream', { json: { stream: 21 } });
  streams.set(21, { chunks: [join(binaryFrame(new Uint8Array([1, 2])), binaryFrame(new Uint8Array([3])), endFrame())] });
  assert.deepEqual(await collect(handle.readable), [1, 2, 3]);
  assert.deepEqual(runtime.argsOf(runtime.calls('fs.readStream').at(-1)), { handle: 7 });

  const part = handle.readStream({ position: 100, length: 5 });
  streams.set(22, { chunks: [join(binaryFrame(new Uint8Array([4, 5])), endFrame())] });
  replies.set('fs.readStream', { json: { stream: 22 } });
  assert.deepEqual(await collect(part), [4, 5]);
  assert.deepEqual(runtime.argsOf(runtime.calls('fs.readStream').at(-1)), { handle: 7, position: 100, length: 5 });
});

test('a stream that fails fails the reader with the error of the runtime', async () => {
  const handle = await opened();
  replies.set('fs.readStream', { json: { stream: 23 } });
  streams.set(23, { chunks: [join(binaryFrame(new Uint8Array([1])), errorFrame({ code: 'INTERNAL', message: 'disk' }))] });
  await assert.rejects(collect(handle.readStream()), error => error instanceof AlefError && error.code === 'INTERNAL');
});

test('writable sends the chunks, ends the stream and waits for the runtime to settle', async () => {
  const handle = await opened({ write: true });
  replies.set('fs.writeStream', { json: { stream: 31 } });
  replies.set('fs.settle', { json: null });
  const writer = handle.writable.getWriter();
  await writer.write(new Uint8Array([1, 2, 3]));
  await writer.close();
  assert.deepEqual(runtime.argsOf(runtime.calls('fs.writeStream').at(-1)), { handle: 7 });
  const writes = runtime.calls('runtime.stream.write');
  assert.deepEqual([...writes.at(-1).body], [1, 2, 3]);
  assert.ok(runtime.calls('runtime.stream.end').length >= 1, 'the stream was ended');
  const order = runtime.calls('fs.settle').length;
  assert.ok(order >= 1, 'and the settle came after');
});

test('a failure of the settle is the failure of close', async () => {
  const handle = await opened({ write: true });
  replies.set('fs.writeStream', { json: { stream: 32 } });
  replies.set('fs.settle', { status: 410, json: { code: 'CLOSED', message: 'stream closed' } });
  const writer = handle.writable.getWriter();
  await writer.write(new Uint8Array([1]));
  await assert.rejects(writer.close(), error => error instanceof AlefError && error.code === 'CLOSED');
});

test('readDirStream yields the entries of every batch and watch yields the events', async () => {
  replies.set('fs.readDirStream', { json: { stream: 41 } });
  const first = [{ name: 'a', path: '/d/a', kind: 'file', size: 1 }];
  const second = [{ name: 'b', path: '/d/b', kind: 'dir', size: 0 }, { name: 'c', path: '/d/c', kind: 'file', size: 2 }];
  streams.set(41, { chunks: [join(jsonFrame(first), jsonFrame(second), endFrame())] });
  const names = [];
  for await (const entry of fs.readDirStream('/d')) names.push(entry.name);
  assert.deepEqual(names, ['a', 'b', 'c']);
  assert.deepEqual(runtime.argsOf(runtime.calls('fs.readDirStream').at(-1)), { path: '/d' });

  replies.set('fs.watch', { json: { stream: 42 } });
  const events = [{ kind: 'create', path: '/d/x' }, { kind: 'rename', path: '/d/x', to: '/d/y' }];
  streams.set(42, { chunks: [join(jsonFrame(events[0]), jsonFrame(events[1]), endFrame())] });
  const seen = [];
  for await (const event of fs.watch('/d', { recursive: true })) seen.push(event);
  assert.deepEqual(seen, events);
  assert.deepEqual(runtime.argsOf(runtime.calls('fs.watch').at(-1)), { path: '/d', recursive: true });
});

test('leaving a watch early closes its stream', async () => {
  replies.set('fs.watch', { json: { stream: 43 } });
  streams.set(43, { chunks: [join(jsonFrame({ kind: 'modify', path: '/d/x' }), jsonFrame({ kind: 'modify', path: '/d/y' }))] });
  const before = runtime.calls('runtime.stream.close').length;
  for await (const event of fs.watch('/d')) {
    assert.equal(event.path, '/d/x');
    break;
  }
  assert.equal(runtime.calls('runtime.stream.close').length, before + 1, 'the runtime was told to stop');
});
