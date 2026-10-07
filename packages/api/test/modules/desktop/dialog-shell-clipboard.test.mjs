// SPDX-License-Identifier: MIT OR Apache-2.0
import assert from 'node:assert/strict';
import test from 'node:test';
import { AlefError, clipboard, dialog, shell } from '../../../src/index.ts';
import { DENIED, installRuntime } from '../../fake-runtime.mjs';

const replies = new Map();
const runtime = installRuntime({
  handler(request) {
    return replies.get(request.url.replace('native://call/', '')) ?? { json: null };
  },
});

/** Calls `fn` and returns what the command was sent and what came back. */
async function sent(command, fn, reply = { json: null }) {
  replies.set(command, reply);
  const before = runtime.calls(command).length;
  const result = await fn();
  const calls = runtime.calls(command);
  assert.equal(calls.length, before + 1, `${command} was called once`);
  return { result, request: calls.at(-1), args: runtime.argsOf(calls.at(-1)) };
}

test('dialog.open and dialog.save carry their options and give back the choice', async () => {
  const options = { title: 'Pick', multiple: true, filters: [{ name: 'Images', extensions: ['png'] }], defaultPath: '/home' };
  const picked = await sent('dialog.open', () => dialog.open(options), { json: ['/a.png', '/b.png'] });
  assert.deepEqual(picked.args, options);
  assert.deepEqual(picked.result, ['/a.png', '/b.png']);
  assert.deepEqual((await sent('dialog.open', () => dialog.open(), { json: [] })).result, [], 'a cancelled dialog is an empty list');
  assert.deepEqual((await sent('dialog.open', () => dialog.open())).args, {}, 'no options means an empty object');

  const saved = await sent('dialog.save', () => dialog.save({ defaultPath: '/home/x.txt' }), { json: '/home/x.txt' });
  assert.deepEqual(saved.args, { defaultPath: '/home/x.txt' });
  assert.equal(saved.result, '/home/x.txt');
  assert.equal((await sent('dialog.save', () => dialog.save(), { json: null })).result, null, 'cancelled is null');
});

test('dialog.message and dialog.confirm carry their options', async () => {
  const message = await sent('dialog.message', () => dialog.message({ title: 'Done', message: 'Saved', kind: 'warning' }));
  assert.deepEqual(message.args, { title: 'Done', message: 'Saved', kind: 'warning' });
  assert.equal(message.result, null);
  const options = { message: 'Delete?', okLabel: 'Delete', cancelLabel: 'Keep' };
  const yes = await sent('dialog.confirm', () => dialog.confirm(options), { json: true });
  assert.deepEqual(yes.args, options);
  assert.equal(yes.result, true);
  assert.equal((await sent('dialog.confirm', () => dialog.confirm({ message: 'Again?' }), { json: false })).result, false);
});

test('a refusal of a dialog is an AlefError and the signal reaches fetch', async () => {
  replies.set('dialog.open', { status: 400, json: { code: 'INVALID_ARGUMENT', message: 'dialog.open: title is longer than 256 characters' } });
  await assert.rejects(dialog.open({ title: 'x' }), error => error instanceof AlefError && error.code === 'INVALID_ARGUMENT');
  const controller = new AbortController();
  controller.abort();
  await assert.rejects(dialog.confirm({ message: 'm' }, { signal: controller.signal }), { name: 'AbortError' });
});

test('shell commands carry the address or the path', async () => {
  assert.deepEqual((await sent('shell.openExternal', () => shell.openExternal('https://example.com/docs/a'))).args, { url: 'https://example.com/docs/a' });
  assert.deepEqual((await sent('shell.openPath', () => shell.openPath('/home/a.txt'))).args, { path: '/home/a.txt' });
  assert.deepEqual((await sent('shell.showInFolder', () => shell.showInFolder('/home/a.txt'))).args, { path: '/home/a.txt' });
  const trashed = await sent('shell.trash', () => shell.trash('/home/a.txt'));
  assert.deepEqual(trashed.args, { path: '/home/a.txt' });
  assert.equal(trashed.result, null);
});

test('a denied shell command rejects with PERMISSION_DENIED', async () => {
  replies.set('shell.openExternal', { status: 403, json: DENIED });
  await assert.rejects(shell.openExternal('https://evil.example/'), error => error instanceof AlefError && error.code === 'PERMISSION_DENIED');
});

test('text and html travel as the bytes of a body and come back decoded', async () => {
  const text = 'Привет, мир — שלום 🙂';
  const written = await sent('clipboard.writeText', () => clipboard.writeText(text));
  assert.equal(written.args, null, 'the content is the body, not an argument');
  assert.equal(written.request.headers['content-type'], 'application/octet-stream');
  assert.deepEqual(written.request.body, new TextEncoder().encode(text));
  assert.deepEqual((await sent('clipboard.writeHtml', () => clipboard.writeHtml('<b>x</b>'))).request.body, new TextEncoder().encode('<b>x</b>'));
  assert.deepEqual((await sent('clipboard.writeText', () => clipboard.writeText(''))).request.body, new Uint8Array(0), 'empty text is still a body');

  assert.equal((await sent('clipboard.readText', () => clipboard.readText(), { bytes: new TextEncoder().encode(text) })).result, text);
  assert.equal((await sent('clipboard.readHtml', () => clipboard.readHtml(), { bytes: new TextEncoder().encode('<i>y</i>') })).result, '<i>y</i>');
  assert.equal((await sent('clipboard.readText', () => clipboard.readText(), { bytes: new Uint8Array(0) })).result, '', 'no text reads as an empty string');
  const withMark = new Uint8Array([0xEF, 0xBB, 0xBF, 0x61]);
  assert.equal((await sent('clipboard.readText', () => clipboard.readText(), { bytes: withMark })).result, '﻿a', 'a byte order mark is text');
});

test('images travel as PNG bytes, and no image is null', async () => {
  const png = new Uint8Array([0x89, 0x50, 0x4E, 0x47, 1, 2, 3]);
  const written = await sent('clipboard.writeImage', () => clipboard.writeImage(png));
  assert.equal(written.request.body, png);
  assert.equal(written.args, null);
  const read = await sent('clipboard.readImage', () => clipboard.readImage(), { bytes: png });
  assert.deepEqual(read.result, png);
  assert.equal((await sent('clipboard.readImage', () => clipboard.readImage(), { json: null })).result, null);
});

test('reading without the permission rejects, and the signal reaches fetch', async () => {
  replies.set('clipboard.readText', { status: 403, json: DENIED });
  await assert.rejects(clipboard.readText(), error => error instanceof AlefError && error.code === 'PERMISSION_DENIED');
  const controller = new AbortController();
  controller.abort();
  await assert.rejects(clipboard.writeText('x', { signal: controller.signal }), { name: 'AbortError' });
});
