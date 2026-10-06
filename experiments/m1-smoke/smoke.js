// M1 transport smoke (docs/stages/m1-core.md): drives native://call and native://stream inside
// Servo, both with raw fetch (the protocol) and through the real @alef-tron/api (transpiled by
// run.mjs next to this page), and reports to the runtime stderr through `smoke.report`; run.mjs
// reads the verdict.
import * as api from './src/index.js';

const bootstrap = new URLSearchParams(location.hash.slice(1)).get('capability') ?? '';
const logElement = document.getElementById('log');
const sleep = ms => new Promise(resolve => setTimeout(resolve, ms));
const MIB = 1024 * 1024;

let token = '';
const results = [];

const log = line => { logElement.textContent += `${line}\n`; };
const bearer = value => ({ Authorization: `Bearer ${value}` });

async function call(name, args = {}, { body, signal, as = token } = {}) {
  const headers = { ...bearer(as) };
  const init = { method: 'POST', headers, signal };
  if (body) {
    headers['Content-Type'] = 'application/octet-stream';
    headers['x-alef-args'] = encodeURIComponent(JSON.stringify(args));
    init.body = body;
  } else {
    headers['Content-Type'] = 'application/json';
    init.body = JSON.stringify(args);
  }
  return fetch(`native://call/${name}`, init);
}

async function callJson(name, args, options) {
  const response = await call(name, args, options);
  const value = await response.json();
  if (!response.ok) throw Object.assign(new Error(value.message ?? 'call failed'), { status: response.status, value });
  return value;
}

async function report(line) {
  await fetch('native://call/smoke.report', {
    method: 'POST',
    headers: { ...bearer(token || bootstrap), 'Content-Type': 'application/json' },
    body: JSON.stringify({ line }),
  }).catch(() => {});
}

// Frame stream: [kind u8][len u32 LE][payload]; 1 json, 2 binary, 3 end, 4 error.
class FrameStream {
  constructor(response) {
    this.reader = response.body.getReader();
    this.buffer = new Uint8Array(0);
    this.pending = null;
    this.finished = false;
  }

  async #frame() {
    for (;;) {
      if (this.buffer.length >= 5) {
        const length = new DataView(this.buffer.buffer, this.buffer.byteOffset + 1, 4).getUint32(0, true);
        if (this.buffer.length >= 5 + length) {
          const frame = { kind: this.buffer[0], payload: this.buffer.slice(5, 5 + length) };
          this.buffer = this.buffer.slice(5 + length);
          return frame;
        }
      }
      const { done, value } = await this.reader.read();
      if (done) return null;
      const merged = new Uint8Array(this.buffer.length + value.length);
      merged.set(this.buffer);
      merged.set(value, this.buffer.length);
      this.buffer = merged;
    }
  }

  // Next frame, or `undefined` when nothing arrives within `ms`; `null` at end of body.
  async next(ms) {
    this.pending ??= this.#frame();
    const winner = await Promise.race([this.pending, sleep(ms).then(() => undefined)]);
    if (winner !== undefined) this.pending = null;
    return winner;
  }

  json(frame) { return JSON.parse(new TextDecoder().decode(frame.payload)); }
  cancel() { return this.reader.cancel().catch(() => {}); }
}

async function openStream(id, options = {}) {
  const response = await fetch(`native://stream/${id}`, { headers: bearer(token), signal: options.signal });
  if (!response.ok) throw new Error(`stream ${id}: HTTP ${response.status}`);
  return new FrameStream(response);
}

async function check(name, body) {
  const started = performance.now();
  try {
    const detail = await body();
    results.push({ name, ok: true });
    await report(`check ${name} ok ${Math.round(performance.now() - started)}ms ${detail ?? ''}`);
  } catch (error) {
    results.push({ name, ok: false });
    console.error(`M1_SMOKE-page check ${name} FAILED ${error?.message ?? error}`);
    await report(`check ${name} FAILED ${error?.message ?? error}`);
  }
}

function pattern(size) {
  const bytes = new Uint8Array(size);
  for (let i = 0; i < size; i += 1) bytes[i] = (i * 31 + (i >> 8)) & 255;
  return bytes;
}

const same = (left, right) => left.length === right.length && left.every((byte, index) => byte === right[index]);

async function until(predicate, ms, what) {
  const deadline = performance.now() + ms;
  while (!predicate()) {
    if (performance.now() > deadline) throw new Error(`timed out waiting for ${what}`);
    await sleep(25);
  }
}

async function rawChecks() {
  await check('hello', async () => {
    const response = await fetch('native://call/runtime.hello', { method: 'POST', headers: { ...bearer(bootstrap), 'Content-Type': 'application/json' }, body: '{}' });
    const info = await response.json();
    if (!response.ok || info.protocol !== 1 || typeof info.token !== 'string' || info.token.length !== 64) throw new Error(`bad hello ${response.status}`);
    for (const module of ['app', 'window', 'runtime', 'smoke']) if (!info.modules.includes(module)) throw new Error(`module ${module} missing`);
    token = info.token;
    return `modules=${info.modules}`;
  });
  await check('denials', async () => {
    const none = await fetch('native://call/smoke.echo', { method: 'POST', body: '{}', headers: { 'Content-Type': 'application/json' } });
    const wrong = await call('smoke.echo', {}, { as: 'wrong-token' });
    const asBootstrap = await call('smoke.echo', {}, { as: bootstrap });
    if (none.status !== 403 || wrong.status !== 403 || asBootstrap.status !== 403) throw new Error(`${none.status}/${wrong.status}/${asBootstrap.status}`);
  });
  await check('json-echo', async () => {
    const args = { a: [1, 2, 3], s: 'привет', n: null };
    const value = await callJson('smoke.echo', args);
    if (JSON.stringify(value) !== JSON.stringify(args)) throw new Error('mismatch');
  });
  await check('binary-echo-16MiB', async () => {
    const data = pattern(16 * MIB);
    const response = await call('smoke.echo', { name: 'ключ' }, { body: data });
    const back = new Uint8Array(await response.arrayBuffer());
    if (back.length !== data.length) throw new Error(`length ${back.length}`);
    for (let i = 0; i < data.length; i += 1) if (back[i] !== data[i]) throw new Error(`byte ${i}`);
    return `${back.length} bytes intact`;
  });
  await check('app-command-on-registry', async () => {
    const hello = await callJson('app.hello', null);
    if (hello.engine !== 'Servo 0.6.0') throw new Error('unexpected reply');
  });
  await check('legacy-invoke-route-is-gone', async () => {
    const legacy = await fetch('native://invoke/', { method: 'POST', headers: { ...bearer(bootstrap), 'Content-Type': 'application/json' }, body: JSON.stringify({ command: 'hello', arguments: null }) });
    if (legacy.status !== 404) throw new Error(`native://invoke/ answered ${legacy.status}`);
  });
  await check('credit-window', async () => {
    const total = 4 * MIB;
    const { stream } = await callJson('smoke.flood', { total, piece: 64 * 1024 });
    const frames = await openStream(stream);
    let received = 0;
    let acked = 0;
    let stalls = 0;
    for (;;) {
      const frame = await frames.next(400);
      if (frame === undefined) {
        // silence: the producer must be waiting for credit right now
        if (received - acked !== MIB) throw new Error(`stalled with ${received - acked} unacked bytes`);
        stalls += 1;
        await call('runtime.stream.ack', { id: stream, bytes: received - acked });
        acked = received;
      } else if (frame === null || frame.kind === 3) {
        break;
      } else if (frame.kind === 2) {
        received += frame.payload.length;
        if (received - acked > MIB) throw new Error(`window exceeded: ${received - acked}`);
      } else {
        throw new Error(`unexpected frame ${frame.kind}`);
      }
    }
    if (received !== total || stalls < 3) throw new Error(`received ${received}, stalls ${stalls}`);
    return `stalls=${stalls}`;
  });
  await check('abort-closes-the-source', async () => {
    const { stream } = await callJson('smoke.flood', { total: 256 * MIB, piece: 64 * 1024 });
    const controller = new AbortController();
    const frames = await openStream(stream, { signal: controller.signal });
    const first = await frames.next(2000);
    if (!first || first.kind !== 2) throw new Error('no data');
    await report(`abort-called stream=${stream} at_ms=${Date.now()}`);
    controller.abort();
    await call('runtime.stream.close', { id: stream });
    await sleep(400);
  });
  await check('events-stream', async () => {
    const { stream } = await callJson('runtime.events.subscribe', {});
    const frames = await openStream(stream);
    await callJson('app.hello', null); // emits `backend.greeting`
    const seen = new Set();
    const deadline = performance.now() + 8000;
    await call('window.apply', { action: 'maximize' });
    while (performance.now() < deadline && !(seen.has('backend.greeting') && seen.has('runtime.window.state'))) {
      const frame = await frames.next(500);
      if (frame && frame.kind === 1) seen.add(frames.json(frame).name);
    }
    await call('window.apply', { action: 'restore' });
    await frames.cancel();
    if (!seen.has('backend.greeting') || !seen.has('runtime.window.state')) throw new Error(`events seen: ${[...seen]}`);
    return `events=${[...seen]}`;
  });
}

// The same behaviours through the real client library.
async function libraryChecks() {
  await check('lib-connect', async () => {
    const info = await api.connect();
    if (info.protocol !== 1 || 'token' in info || !info.modules.includes('smoke')) throw new Error(JSON.stringify(info));
    return `runtime=${info.runtime}`;
  });
  await check('lib-call-json', async () => {
    const args = { a: [1, 2, 3], s: 'привет', n: null };
    const value = await api.call('smoke.echo', args);
    if (JSON.stringify(value) !== JSON.stringify(args)) throw new Error('mismatch');
  });
  await check('lib-binary-roundtrip-4MiB', async () => {
    const data = pattern(4 * MIB);
    const back = await api.call('smoke.echo', { name: 'ключ' }, { body: data });
    if (!(back instanceof Uint8Array) || !same(back, data)) throw new Error('bytes differ');
    return `${back.length} bytes intact`;
  });
  await check('lib-error-mapping', async () => {
    const error = await api.call('smoke.nope').then(() => null, reason => reason);
    if (!(error instanceof api.AlefError) || error.code !== 'NOT_FOUND' || error.status !== 404) throw new Error(String(error));
  });
  await check('lib-readable-acks-by-itself', async () => {
    const total = 8 * MIB;
    const { stream } = await api.call('smoke.flood', { total, piece: 64 * 1024 });
    const readable = await api.openReadable(stream);
    let received = 0;
    for await (const frame of readable) {
      if (frame.kind !== 'binary') throw new Error(`unexpected ${frame.kind} frame`);
      received += frame.data.length;
    }
    if (received !== total) throw new Error(`received ${received} of ${total}`);
    return `${received} bytes`;
  });
  await check('lib-close-stops-the-source', async () => {
    const { stream } = await api.call('smoke.flood', { total: 256 * MIB, piece: 64 * 1024 });
    const readable = await api.openReadable(stream);
    const first = await readable[Symbol.asyncIterator]().next();
    if (first.done || first.value.kind !== 'binary') throw new Error('no data');
    await report(`abort-called stream=${stream} at_ms=${Date.now()}`);
    await readable.close();
    await sleep(400);
  });
  await check('lib-events', async () => {
    const seen = [];
    const off = await api.on('backend.greeting', payload => seen.push(payload));
    await api.call('app.hello', null);
    await until(() => seen.length > 0, 5000, 'backend.greeting');
    off();
    return `payload engine=${seen[0].engine}`;
  });
  await check('lib-window-watch', async () => {
    const states = [];
    const stop = await api.nativeWindow.watch(state => states.push(state));
    if (states.length === 0 || typeof states[0].revision !== 'number') throw new Error('no snapshot');
    await api.nativeWindow.maximize();
    await until(() => states.some(state => state.maximized), 8000, 'a maximized window state');
    await api.nativeWindow.restore();
    stop();
    const revisions = states.map(state => state.revision);
    if (revisions.some((revision, index) => index > 0 && revision <= revisions[index - 1])) throw new Error(`revisions ${revisions}`);
    return `revisions=${revisions}`;
  });
}

async function newDocumentChecks(previous, how) {
  await check(`${how}-new-token`, async () => {
    const response = await fetch('native://call/runtime.hello', { method: 'POST', headers: { ...bearer(bootstrap), 'Content-Type': 'application/json' }, body: '{}' });
    const info = await response.json();
    if (!response.ok || info.token === previous.token) throw new Error(`hello after ${how}: ${response.status}`);
    token = info.token;
  });
  await check(`${how}-old-token-denied`, async () => {
    const old = await call('smoke.echo', {}, { as: previous.token });
    if (old.status !== 403) throw new Error(`old token status ${old.status}`);
    const fresh = await call('smoke.echo', { ok: true });
    if (!fresh.ok) throw new Error(`new token status ${fresh.status}`);
  });
  await check(`${how}-library-reconnects`, async () => {
    const echoed = await api.call('smoke.echo', { after: how });
    if (echoed.after !== how) throw new Error('library call failed in the new document');
  });
}

// State crosses documents in the URL (Servo drops `window.name`, custom-scheme origins are opaque):
// phase 2 arrives by navigation (query changes), phase 3 by reload of a fragment-only rewrite.
function readState() {
  const hash = new URLSearchParams(location.hash.slice(1));
  const source = hash.has('phase') ? hash : new URLSearchParams(location.search);
  return {
    phase: Number(source.get('phase') ?? 1),
    token: source.get('token') ?? '',
    failed: (source.get('failed') ?? '').split(',').filter(Boolean),
  };
}

const failedNow = () => results.filter(item => !item.ok).map(item => item.name);

async function main() {
  const state = readState();
  if (state.phase === 1) {
    await rawChecks();
    await libraryChecks();
    await report('navigating');
    location.search = `?${new URLSearchParams({ phase: '2', token, failed: failedNow().join(',') })}`;
    return;
  }
  const how = state.phase === 2 ? 'navigation' : 'reload';
  await newDocumentChecks(state, how);
  const failed = [...state.failed, ...failedNow()];
  if (state.phase === 2) {
    await report('reloading');
    history.replaceState(null, '', `#${new URLSearchParams({ capability: bootstrap, phase: '3', token, failed: failed.join(',') })}`);
    location.reload();
    return;
  }
  log(failed.length === 0 ? 'PASS' : `FAIL ${failed}`);
  await report(`RESULT ${failed.length === 0 ? 'PASS' : 'FAIL'} ${failed.join(',')}`);
}

setTimeout(() => report('RESULT FAIL timeout'), 120000);
main().catch(error => {
  console.error(`M1_SMOKE-page fatal ${error?.message ?? error}`);
  return report(`RESULT FAIL ${error?.message ?? error}`);
});
