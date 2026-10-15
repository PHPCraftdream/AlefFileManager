// SPDX-License-Identifier: MIT OR Apache-2.0
// The `ai` segment against the fake runtime: roles, the chat stream, tool calls and their results.
// Every wait is bounded: a test fails, it never hangs.
import assert from 'node:assert/strict';
import test from 'node:test';
import { AlefError, ai } from '../../../src/index.ts';
import { on } from '../../../src/core/events.ts';
import { barrier, installRuntime, jsonFrame, liveStream } from '../../fake-runtime.mjs';

const feed = liveStream(); // the stream of `runtime.events.subscribe`
const replies = new Map(); // command -> fake reply
const opened = new Map(); // stream id -> the bodies handed out, one per open; the test feeds the last
const runtime = installRuntime({
  handler(request) {
    if (request.url === 'native://call/runtime.events.subscribe') return { json: { stream: 5 } };
    if (request.url === 'native://stream/5') return feed.reply;
    const id = /^native:\/\/stream\/(\d+)$/.exec(request.url)?.[1];
    if (id !== undefined) {
      const list = opened.get(id) ?? [];
      list.push(liveStream()); // a body may be read once: every open gets a fresh one
      opened.set(id, list);
      return list.at(-1).reply;
    }
    return replies.get(request.url.replace('native://call/', '')) ?? { json: {} };
  },
});

const settle = () => barrier(feed, on, 'test.ai.marker');
const args = name => runtime.calls(name).map(runtime.argsOf);
const stream = id => (opened.get(String(id)) ?? []).at(-1);
/** Waits for a fresh open of `id` (previous bodies of the id are already used up). */
const openStream = async id => {
  const seen = (opened.get(id) ?? []).length;
  await waitFor(() => (opened.get(id) ?? []).length > seen);
  return stream(id);
};

/** Polls `check` until it holds or `limit` ms pass; a missed condition fails the test. */
async function waitFor(check, limit = 2000) {
  const deadline = Date.now() + limit;
  while (!check()) {
    if (Date.now() > deadline) throw new Error('the expected runtime exchange never happened');
    await new Promise(resolve => setTimeout(resolve, 10));
  }
}

const bounded = (name, fn) => test(name, { timeout: 5000 }, fn);
const chatArgs = { role: 'coder', messages: [{ role: 'user', content: 'hi' }] };

bounded('roles returns the roles the runtime lists', async () => {
  replies.set('ai.roles', { json: { roles: [{ id: 'coder', title: 'Coder', description: 'writes code', capabilities: ['text', 'tool_calls'] }] } });
  assert.deepEqual(await ai.roles(), [
    { id: 'coder', title: 'Coder', description: 'writes code', capabilities: ['text', 'tool_calls'] },
  ]);
  assert.deepEqual(args('ai.roles'), [null]);
});

bounded('chat streams parts and ends exactly once', async () => {
  replies.set('ai.chat', { json: { chat: 7, stream: 71 } });
  const parts = [];
  const done = (async () => {
    for await (const part of ai.chat(chatArgs)) parts.push(part);
  })();
  const body = await openStream('71');
  body.push(jsonFrame({ type: 'text', text: 'hello' }));
  stream(71).push(jsonFrame({
    type: 'usage', inputTokens: 3, outputTokens: 5,
    cacheReadTokens: null, cacheCreationTokens: null, estimated: true, complete: true,
  }));
  stream('71').push(jsonFrame({ type: 'end', reason: 'completed' }));
  stream('71').end(); // the runtime closes the stream after the end part
  await done;
  assert.deepEqual(parts, [
    { type: 'text', text: 'hello' },
    { type: 'usage', inputTokens: 3, outputTokens: 5, cacheReadTokens: null, cacheCreationTokens: null, estimated: true, complete: true },
    { type: 'end', reason: 'completed' },
  ]);
  assert.deepEqual(args('ai.chat'), [{ role: 'coder', messages: [{ role: 'user', content: 'hi' }], tools: [] }]);
});

bounded('breaking the chat closes the stream', async () => {
  replies.set('ai.chat', { json: { chat: 7, stream: 71 } });
  const parts = [];
  const done = (async () => {
    for await (const part of ai.chat(chatArgs)) {
      parts.push(part);
      break; // the break must close the stream and cancel the chat
    }
  })();
  const body = await openStream('71');
  body.push(jsonFrame({ type: 'text', text: 'first' }));
  await done;
  assert.deepEqual(parts, [{ type: 'text', text: 'first' }]);
  await waitFor(() => args('runtime.stream.close').some(close => close.id === 71));
  assert.equal(body.aborted, true, 'closing the stream cancelled the chat in the runtime');
});

bounded('a tool call runs the handler and reports the result', async () => {
  replies.set('ai.tools.handle', { json: { registration: 81, inert: false } });
  // a PLAIN (non-async) return value: the report must wrap it, not await a thenable
  const unregister = await ai.tools.handle('summarizeSelection', callArgs => ({ echo: callArgs.input }));
  try {
    await settle();
    // a live chat tracks chat 7; without it nothing is dispatched for the chat
    replies.set('ai.chat', { json: { chat: 7, stream: 71 } });
    const consumed = (async () => { for await (const part of ai.chat(chatArgs)) { /* every part is ignored here */ } })();
    const body = await openStream('71');
    feed.push(jsonFrame({ name: 'ai.tool_call', payload: { chat: 7, callId: 'call-1', name: 'summarizeSelection', arguments: { input: 'ping' } } }));
    await waitFor(() => args('ai.tools.result').some(result => result.chat === 7 && result.callId === 'call-1'));
    assert.deepEqual(args('ai.tools.result').at(-1), { chat: 7, callId: 'call-1', name: 'summarizeSelection', result: { echo: 'ping' } });
    body.push(jsonFrame({ type: 'end', reason: 'completed' }));
    body.end();
    await consumed;
  } finally {
    await unregister();
  }
});

bounded('a failing handler reports a cleared error', async () => {
  replies.set('ai.tools.handle', { json: { registration: 83, inert: false } });
  const unregister = await ai.tools.handle('summarizeSelection', async () => {
    throw new Error('secret boom');
  });
  try {
    await settle();
    // a live chat tracks chat 7; without it nothing is dispatched for the chat
    replies.set('ai.chat', { json: { chat: 7, stream: 72 } });
    const consumed = (async () => { for await (const part of ai.chat(chatArgs)) { /* every part is ignored here */ } })();
    const body = await openStream('72');
    feed.push(jsonFrame({ name: 'ai.tool_call', payload: { chat: 7, callId: 'call-2', name: 'summarizeSelection', arguments: {} } }));
    await waitFor(() => args('ai.tools.result').some(result => result.callId === 'call-2'));
    assert.deepEqual(args('ai.tools.result').at(-1), { chat: 7, callId: 'call-2', name: 'summarizeSelection', error: 'the tool handler failed' });
    const report = args('ai.tools.result').at(-1);
    assert.equal(report.error, 'the tool handler failed');
    assert.equal(JSON.stringify(report).includes('secret boom'), false, 'the raw error must not reach the runtime');
    body.push(jsonFrame({ type: 'end', reason: 'completed' }));
    body.end();
    await consumed;
  } finally {
    await unregister();
  }
});

bounded('a duplicate registration is busy', async () => {
  replies.set('ai.tools.handle', { status: 409, json: { code: 'BUSY', message: 'already registered' } });
  await assert.rejects(ai.tools.handle('summarizeSelection', async () => null), error =>
    error instanceof AlefError && error.code === 'BUSY');
});

bounded('a denied registration is permission denied', async () => {
  replies.set('ai.tools.handle', { status: 403, json: { code: 'PERMISSION_DENIED', message: 'denied' } });
  await assert.rejects(ai.tools.handle('summarizeSelection', async () => null), error =>
    error instanceof AlefError && error.code === 'PERMISSION_DENIED');
});

bounded('unregister stops dispatching, aborts its handlers and is idempotent', async () => {
  replies.set('ai.tools.handle', { json: { registration: 82, inert: false } });
  let started = 0;
  const unregister = await ai.tools.handle('summarizeSelection', (_callArgs, context) => {
    started += 1;
    return new Promise((_resolve, _reject) => {
      context.signal.addEventListener('abort', () => { /* the unregister aborts its pending handlers */ });
    });
  });
  try {
    await settle();
    // a live chat keeps the chat tracked while the body stays open
    replies.set('ai.chat', { json: { chat: 7, stream: 73 } });
    const consumed = (async () => { for await (const part of ai.chat(chatArgs)) { /* every part is ignored here */ } })();
    const body = await openStream('73');
    feed.push(jsonFrame({ name: 'ai.tool_call', payload: { chat: 7, callId: 'call-5', name: 'summarizeSelection', arguments: {} } }));
    await waitFor(() => started === 1);
    const unhandles = args('ai.tools.unhandle').length;
    await unregister();
    assert.deepEqual(args('ai.tools.unhandle').slice(unhandles), [{ registration: 82 }]);
    // a second call for the same chat after the unregister must not reach the handler
    feed.push(jsonFrame({ name: 'ai.tool_call', payload: { chat: 7, callId: 'call-6', name: 'summarizeSelection', arguments: {} } }));
    await settle();
    assert.equal(started, 1, 'the handler must not run after the unregister');
    assert.equal(args('ai.tools.result').filter(result => result.callId === 'call-6').length, 0, 'no result request for call-6');
    await unregister(); // a second call resolves without another runtime exchange
    assert.equal(args('ai.tools.unhandle').length, unhandles + 1, 'exactly one unhandle request was added by this test');
    // end the chat so the still-pending first handler is aborted and the test ends clean
    body.push(jsonFrame({ type: 'end', reason: 'completed' }));
    body.end();
    await consumed;
  } finally {
    await unregister();
  }
});

bounded('chat rejects invalid arguments before calling the runtime', () => {
  const before = runtime.requests.length;
  for (const bad of [
    { role: '', messages: [{ role: 'user', content: 'hi' }] },
    { role: 'coder', messages: [] },
    { role: 'coder', messages: [{ role: 'system', content: 'hi' }] },
    { role: 'coder', messages: [{ role: 'user', content: 5 }] },
    { role: 'coder' },
  ]) {
    assert.throws(() => ai.chat(bad), error => error instanceof AlefError && error.code === 'INVALID_ARGUMENT');
  }
  assert.equal(runtime.requests.length, before, 'no runtime request for invalid arguments');
});

bounded('a tool call for an ended chat is not dispatched', async () => {
  replies.set('ai.tools.handle', { json: { registration: 84, inert: false } });
  let runs = 0;
  const unregister = await ai.tools.handle('summarizeSelection', () => { runs += 1; return null; });
  try {
    await settle();
    // a REAL chat that runs to its end: the iterator finishing untracks chat 7
    replies.set('ai.chat', { json: { chat: 7, stream: 74 } });
    const consumed = (async () => { for await (const part of ai.chat(chatArgs)) { /* every part is ignored here */ } })();
    const body = await openStream('74');
    body.push(jsonFrame({ type: 'end', reason: 'completed' }));
    body.end();
    await consumed; // the iterator finished: chat 7 is untracked
    feed.push(jsonFrame({ name: 'ai.tool_call', payload: { chat: 7, callId: 'call-4', name: 'summarizeSelection', arguments: {} } }));
    await settle();
    assert.equal(runs, 0, 'the handler must not run for a chat that already ended');
    assert.equal(args('ai.tools.result').filter(result => result.callId === 'call-4').length, 0, 'no result request');
  } finally {
    await unregister();
  }
});

bounded('a synchronous throw reports a cleared error', async () => {
  replies.set('ai.tools.handle', { json: { registration: 85, inert: false } });
  const unregister = await ai.tools.handle('summarizeSelection', () => {
    throw new Error('sync boom');
  });
  try {
    await settle();
    // a live chat tracks chat 7; without it nothing is dispatched for the chat
    replies.set('ai.chat', { json: { chat: 7, stream: 75 } });
    const consumed = (async () => { for await (const part of ai.chat(chatArgs)) { /* every part is ignored here */ } })();
    const body = await openStream('75');
    feed.push(jsonFrame({ name: 'ai.tool_call', payload: { chat: 7, callId: 'call-7', name: 'summarizeSelection', arguments: {} } }));
    await waitFor(() => args('ai.tools.result').some(result => result.callId === 'call-7'));
    assert.deepEqual(args('ai.tools.result').at(-1), { chat: 7, callId: 'call-7', name: 'summarizeSelection', error: 'the tool handler failed' });
    assert.equal(JSON.stringify(runtime.calls('ai.tools.result')).includes('sync boom'), false, 'the raw sync error must not reach the runtime');
    body.push(jsonFrame({ type: 'end', reason: 'completed' }));
    body.end();
    await consumed;
  } finally {
    await unregister();
  }
});
