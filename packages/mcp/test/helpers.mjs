// SPDX-License-Identifier: MIT OR Apache-2.0
import { bounded } from '../src/transports/body.ts';
export const wait = promise => bounded(promise, 1000);
export const encode = text => new TextEncoder().encode(text);
export function memory() {
  const sides = [{}, {}];
  return sides.map((side, index) => ({
    async send(message) { if (side.closed) throw new Error('Closed.'); sides[1 - index].receive?.(structuredClone(message)); },
    start(receive, end) { Object.assign(side, { receive, end }); },
    async close() { if (side.closed) return; side.closed = true; side.end?.(); sides[1 - index].end?.(); },
  }));
}
export function queue() {
  const values = [];
  let waiting;
  let closed = false;
  return {
    push(value) { if (waiting) { const resolve = waiting; waiting = undefined; resolve({ value, done: false }); } else values.push(value); },
    close() { closed = true; waiting?.({ done: true }); waiting = undefined; },
    [Symbol.asyncIterator]() { return this; },
    next() { if (values.length) return Promise.resolve({ value: values.shift(), done: false }); if (closed) return Promise.resolve({ done: true }); return new Promise(resolve => { waiting = resolve; }); },
  };
}
export function fakeHttp() {
  const requests = queue();
  let closed = false;
  const native = { address: { host: '127.0.0.1', port: 1234 }, url: 'http://127.0.0.1:1234', [Symbol.asyncIterator]: () => requests, async close() { closed = true; requests.close(); } };
  return {
    native,
    get closed() { return closed; },
    request(method, message, headers = {}, url = '/mcp') {
      return wait(new Promise(resolve => requests.push({
        method, url, headers: new Headers({ host: '127.0.0.1:1234', 'content-type': 'application/json', accept: 'application/json, text/event-stream', ...headers }),
        body: message === undefined ? null : new ReadableStream({ start(controller) { controller.enqueue(encode(typeof message === 'string' ? message : JSON.stringify(message))); controller.close(); } }),
        async respond(response) { resolve({ status: response.status, headers: new Headers(response.headers), body: response.body ? JSON.parse(response.body) : undefined }); },
      })));
    },
  };
}
export const initialize = (version = '2025-11-25') => ({ jsonrpc: '2.0', id: 1, method: 'initialize', params: { protocolVersion: version, capabilities: {}, clientInfo: { name: 'test', version: '1' } } });
