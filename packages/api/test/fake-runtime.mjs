// SPDX-License-Identifier: MIT OR Apache-2.0
// A fake native runtime for the @alef-tron/api tests: `fetch` and `location` stand in for Servo.
const encoder = new TextEncoder();

export const INFO = {
  protocol: 1,
  runtime: '0.0.0-test',
  platform: 'test',
  arch: 'test',
  modules: ['runtime', 'window', 'app'],
  limits: {
    maxUnaryBody: 1048576,
    maxBulkBody: 67108864,
    streamWindow: 1048576,
    chunkSize: 4,
    maxResources: 64,
    maxConcurrentCalls: 32,
  },
};

export const DENIED = { code: 'PERMISSION_DENIED', message: 'permission denied' };

export function frame(kind, payload) {
  const out = new Uint8Array(5 + payload.length);
  out[0] = kind;
  new DataView(out.buffer).setUint32(1, payload.length, true);
  out.set(payload, 5);
  return out;
}
export const jsonFrame = value => frame(1, encoder.encode(JSON.stringify(value)));
export const binaryFrame = bytes => frame(2, bytes);
export const endFrame = () => frame(3, new Uint8Array(0));
export const errorFrame = body => frame(4, encoder.encode(JSON.stringify(body)));

export function join(...parts) {
  const out = new Uint8Array(parts.reduce((total, part) => total + part.length, 0));
  let at = 0;
  for (const part of parts) {
    out.set(part, at);
    at += part.length;
  }
  return out;
}

/** Splits bytes into pieces of `size`; the decoder must not care where the boundaries fall. */
export function chunked(bytes, size) {
  const pieces = [];
  for (let at = 0; at < bytes.length; at += size) pieces.push(bytes.subarray(at, at + size));
  return pieces;
}

/** A stream body the test feeds by hand; aborting the request errors it like a real fetch. */
export function liveStream() {
  let controller;
  let aborted = false;
  const body = new ReadableStream({ start(c) { controller = c; } });
  return {
    reply: {
      body,
      attach(signal) {
        signal?.addEventListener('abort', () => {
          aborted = true;
          controller.error(new DOMException('aborted', 'AbortError'));
        }, { once: true });
      },
    },
    push: bytes => controller.enqueue(bytes),
    end: () => controller.close(),
    get aborted() { return aborted; },
  };
}

function respond(reply, signal) {
  const status = reply.status ?? 200;
  const headers = new Headers(reply.headers ?? {});
  if (reply.body) {
    reply.attach?.(signal);
    return new Response(reply.body, { status, headers });
  }
  if (reply.chunks) {
    let index = 0;
    const body = new ReadableStream({
      pull(controller) {
        if (index < reply.chunks.length) controller.enqueue(reply.chunks[index++]);
        else controller.close();
      },
    });
    return new Response(body, { status, headers });
  }
  if (reply.bytes) {
    headers.set('content-type', 'application/octet-stream');
    return new Response(reply.bytes, { status, headers });
  }
  headers.set('content-type', 'application/json');
  return new Response(reply.text ?? JSON.stringify('json' in reply ? reply.json : {}), { status, headers });
}

/**
 * Installs `location` and `fetch`. `handler(request)` answers everything except the handshake and
 * the authorization check; it returns `{ json | bytes | text | chunks | body, status }`.
 */
export function installRuntime({ hash = '#capability=boot', token = 'tok', handler = () => undefined } = {}) {
  const requests = [];
  globalThis.location = { hash };
  globalThis.fetch = async (url, init = {}) => {
    const headers = Object.fromEntries(
      Object.entries(init.headers ?? {}).map(([name, value]) => [name.toLowerCase(), value]),
    );
    const request = { url: String(url), method: init.method ?? 'GET', headers, body: init.body, signal: init.signal };
    requests.push(request);
    if (init.signal?.aborted) throw new DOMException('aborted', 'AbortError');
    let reply;
    if (request.url === 'native://call/runtime.hello') {
      reply = headers.authorization === 'Bearer boot' ? { json: { ...INFO, token } } : { status: 403, json: DENIED };
    } else if (headers.authorization !== `Bearer ${token}`) {
      reply = { status: 403, json: DENIED };
    } else {
      reply = (await handler(request)) ?? { json: {} };
    }
    return respond(reply, init.signal);
  };
  return {
    requests,
    calls: name => requests.filter(request => request.url === `native://call/${name}`),
    // The arguments of a call: x-alef-args for binary bodies, the JSON body otherwise.
    argsOf: request => JSON.parse('x-alef-args' in request.headers
      ? decodeURIComponent(request.headers['x-alef-args'])
      : request.body ?? 'null'),
  };
}
