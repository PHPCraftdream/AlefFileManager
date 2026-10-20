// SPDX-License-Identifier: MIT OR Apache-2.0
import assert from 'node:assert/strict';
import test from 'node:test';
import { AlefError, os } from '../../../src/index.ts';
import { on } from '../../../src/core/events.ts';
import { barrier, installRuntime, jsonFrame, liveStream } from '../../fake-runtime.mjs';

const feed = liveStream();
const replies = new Map();
// Every call the page makes, in order; `runtime.events.subscribe` is recorded at its handler.
const order = [];
const runtime = installRuntime({
  handler(request) {
    if (request.url === 'native://call/runtime.events.subscribe') {
      order.push('runtime.events.subscribe');
      return { json: { stream: 5 } };
    }
    if (request.url === 'native://stream/5') return feed.reply;
    const command = request.url.replace('native://call/', '');
    order.push(command);
    return replies.get(command) ?? { json: {} };
  },
});

const settle = () => barrier(feed, on, 'test.os-power.marker');
const NOT_AVAILABLE = { status: 501, json: { code: 'NOT_AVAILABLE', message: 'no power notifications here' } };

test('suspending_subscription_subscribes_before_starting_the_watcher', async () => {
  order.length = 0;
  const off = await os.on('suspend', () => {});
  const subscribeAt = order.indexOf('runtime.events.subscribe');
  const watchAt = order.indexOf('os.watchPower');
  assert.ok(subscribeAt !== -1 && watchAt !== -1, `both calls happened: ${order.join(' -> ')}`);
  assert.ok(subscribeAt < watchAt, `the subscription came first: ${order.join(' -> ')}`);
  const calls = runtime.calls('os.watchPower');
  assert.equal(calls.length, 1, 'the watcher starts exactly once');
  assert.deepEqual(runtime.argsOf(calls[0]), null, 'os.watchPower travels without arguments');
  off();
});

test('resume_events_reach_only_resume_handlers', async () => {
  const suspends = [];
  const resumes = [];
  const offSuspend = await os.on('suspend', () => suspends.push('suspend'));
  const offResume = await os.on('resume', () => resumes.push('resume'));
  feed.push(jsonFrame({ name: 'os.suspend', payload: null }));
  feed.push(jsonFrame({ name: 'os.resume', payload: null }));
  await settle();
  assert.deepEqual(suspends, ['suspend']);
  assert.deepEqual(resumes, ['resume']);
  offSuspend();
  offResume();
});

test('unsubscribing_stops_the_delivery', async () => {
  const seen = [];
  const off = await os.on('suspend', () => seen.push('suspend'));
  off();
  feed.push(jsonFrame({ name: 'os.suspend', payload: null }));
  await settle();
  assert.deepEqual(seen, [], 'nothing arrives after the unsubscribe');
});

test('a_failing_watch_power_rejects_and_leaves_no_subscription', async () => {
  replies.set('os.watchPower', NOT_AVAILABLE);
  let delivered = 0;
  await assert.rejects(
    os.on('suspend', () => {
      delivered += 1;
    }),
    error => error instanceof AlefError && error.code === 'NOT_AVAILABLE',
  );
  feed.push(jsonFrame({ name: 'os.suspend', payload: null }));
  await settle();
  assert.equal(delivered, 0, 'the handler of the undone subscription is gone');

  // The event stream itself keeps working: a fresh theme subscription still delivers.
  const themes = [];
  const offTheme = await os.on('theme-changed', theme => themes.push(theme));
  feed.push(jsonFrame({ name: 'os.theme-changed', payload: { theme: 'dark' } }));
  await settle();
  assert.deepEqual(themes, ['dark']);
  offTheme();
  assert.ok(runtime.calls('runtime.events.subscribe').length >= 1, 'the stream was subscribed');
});
