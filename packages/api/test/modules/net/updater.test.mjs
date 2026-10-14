// SPDX-License-Identifier: MIT OR Apache-2.0
import assert from 'node:assert/strict';
import test from 'node:test';
import { AlefError, updater } from '../../../src/index.ts';
import { DENIED, endFrame, errorFrame, installRuntime, jsonFrame } from '../../fake-runtime.mjs';

const replies = new Map();
const streams = new Map();
const runtime = installRuntime({
  handler(request) {
    const stream = /^native:\/\/stream\/(\d+)$/.exec(request.url);
    if (stream) return streams.get(Number(stream[1])) ?? { status: 404, json: { code: 'NOT_FOUND', message: 'stream not found' } };
    return replies.get(request.url.replace('native://call/', '')) ?? { json: null };
  },
});

const offered = () => replies.set('updater.download', { json: { stream: 71, resource: 72 } });
const closedWith = () => runtime.calls('updater.close').map(request => runtime.argsOf(request));

test('check asks the runtime without arguments and returns its answer as it is', async () => {
  replies.clear();
  replies.set('updater.check', { json: { available: true, version: '2.0.0', notes: 'fixes' } });
  const before = runtime.calls('updater.check').length;
  assert.deepEqual(await updater.check(), { available: true, version: '2.0.0', notes: 'fixes' });
  const calls = runtime.calls('updater.check');
  assert.equal(calls.length, before + 1);
  assert.deepEqual(runtime.argsOf(calls.at(-1)), {});

  replies.set('updater.check', { json: { available: false } });
  assert.deepEqual(await updater.check(), { available: false });
});

test('download reports every progress frame, resolves after the done frame and closes the resource', async () => {
  replies.clear();
  offered();
  streams.set(71, { chunks: [jsonFrame({ received: 2, total: 4 }), jsonFrame({ received: 4, done: true }), endFrame()] });
  const before = closedWith().length;
  const progress = [];
  await updater.download({ onProgress: value => progress.push(value) });
  assert.deepEqual(progress, [{ received: 2, total: 4 }, { received: 4, total: null }]);
  assert.deepEqual(closedWith().slice(before), [{ id: 72 }]);
});

test('download works without options and a stream that ends before the done frame is a failure', async () => {
  replies.clear();
  offered();
  streams.set(71, { chunks: [jsonFrame({ done: true }), endFrame()] });
  await updater.download();

  streams.set(71, { chunks: [jsonFrame({ received: 1, total: 4 }), endFrame()] });
  const before = closedWith().length;
  await assert.rejects(updater.download(), /before completion/);
  assert.deepEqual(closedWith().slice(before), [{ id: 72 }], 'the resource is closed after a failure too');
});

test('an error of the stream reaches the caller with its code and the resource is still closed', async () => {
  replies.clear();
  offered();
  streams.set(71, { chunks: [jsonFrame({ received: 1, total: 4 }), errorFrame({ code: 'UPDATE_NETWORK', message: 'broke' })] });
  const before = closedWith().length;
  await assert.rejects(updater.download(), error => error instanceof AlefError && error.code === 'UPDATE_NETWORK');
  assert.deepEqual(closedWith().slice(before), [{ id: 72 }]);
});

test('a failing close does not hide the result, and a callback error is the error of the download', async () => {
  replies.clear();
  offered();
  replies.set('updater.close', { status: 403, json: DENIED });
  streams.set(71, { chunks: [jsonFrame({ received: 4, total: 4, done: true }), endFrame()] });
  await updater.download();

  const failure = new Error('callback');
  await assert.rejects(
    updater.download({ onProgress: () => { throw failure; } }),
    error => error === failure,
  );
});

test('an aborted signal fails the download with the abort and no stream is read', async () => {
  replies.clear();
  offered();
  const controller = new AbortController();
  controller.abort();
  const before = runtime.requests.filter(request => request.url === 'native://stream/71').length;
  await assert.rejects(updater.download({ signal: controller.signal }), error => error.name === 'AbortError');
  assert.equal(runtime.requests.filter(request => request.url === 'native://stream/71').length, before);
});

test('a refusal of the call reaches the caller and nothing is closed', async () => {
  replies.clear();
  replies.set('updater.download', { status: 403, json: { code: 'PERMISSION_DENIED', message: 'no' } });
  const before = closedWith().length;
  await assert.rejects(updater.download(), error => error instanceof AlefError && error.code === 'PERMISSION_DENIED');
  assert.equal(closedWith().length, before);
});

test('install asks the runtime without arguments', async () => {
  replies.clear();
  const before = runtime.calls('updater.install').length;
  replies.set('updater.install', { json: null });
  await updater.install();
  const calls = runtime.calls('updater.install');
  assert.equal(calls.length, before + 1);
  assert.deepEqual(runtime.argsOf(calls.at(-1)), {});
});

test('a refusal of the install reaches the caller with its code', async () => {
  replies.clear();
  replies.set('updater.install', { status: 409, json: { code: 'UPDATE_NOT_DOWNLOADED', message: 'no downloaded update is offered' } });
  await assert.rejects(updater.install(), error => error instanceof AlefError && error.code === 'UPDATE_NOT_DOWNLOADED');
});

test('check reports the runtime requirement of the feed as it is', async () => {
  replies.clear();
  replies.set('updater.check', { json: { available: true, version: '2.0.0', runtime: { range: '^2.0.0', satisfied: false } } });
  const before = runtime.calls('updater.check').length;
  assert.deepEqual(await updater.check(), { available: true, version: '2.0.0', runtime: { range: '^2.0.0', satisfied: false } });
  const calls = runtime.calls('updater.check');
  assert.equal(calls.length, before + 1);
  assert.deepEqual(runtime.argsOf(calls.at(-1)), {});

  replies.set('updater.check', { json: { available: false } });
  assert.deepEqual(await updater.check(), { available: false });
});

test('a refusal of the install for an unsatisfied runtime reaches the caller with its code', async () => {
  replies.clear();
  replies.set('updater.install', { status: 400, json: { code: 'UPDATE_RUNTIME_REQUIRED', message: 'the update requires runtime ^2.0.0; install it with `alef runtime install <version> <path-to-alef-binary>`' } });
  await assert.rejects(updater.install(), error => error instanceof AlefError && error.code === 'UPDATE_RUNTIME_REQUIRED');
});
