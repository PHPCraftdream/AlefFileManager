// SPDX-License-Identifier: MIT OR Apache-2.0
import type { HttpRequestOptions } from '../../../api/src/net/http.ts';
import { duration, object, type Transport } from '../types.ts';
import { bounded, readBody } from './body.ts';
export interface Response { status: number; headers: Headers; body: ReadableStream<Uint8Array> | null }
export type Requester = (url: string, options: HttpRequestOptions) => Promise<Response>;
export function httpTransport(url: string, token: string | undefined, timeout: number, request: Requester): Transport {
  duration(timeout);
  const parsed = new URL(url);
  if (!['http:', 'https:'].includes(parsed.protocol) || parsed.username || parsed.password || parsed.hash) throw new Error('MCP requires an HTTP URL without credentials or fragment.');
  let session: string | undefined;
  let revision = '2025-11-25';
  let receive: (value: unknown) => void = () => {};
  let closed = false;
  const active = new Map<string | number, AbortController>();
  const controllers = new Set<AbortController>();
  const headers = () => ({ 'content-type': 'application/json', accept: 'application/json, text/event-stream', 'mcp-protocol-version': revision, ...(token === undefined ? {} : { authorization: `Bearer ${token}` }), ...(session ? { 'mcp-session-id': session } : {}) });
  return {
    cancel(id) { active.get(id)?.abort(); },
    start(callback) { receive = callback; },
    async send(message) {
      if (closed) throw new Error('HTTP transport is closed.');
      const controller = new AbortController(); controllers.add(controller);
      const id = object(message) && (typeof message.id === 'number' || typeof message.id === 'string') ? message.id : undefined;
      if (id !== undefined) active.set(id, controller);
      try {
        const response = await bounded(request(url, { method: 'POST', headers: headers(), body: JSON.stringify(message), timeout, signal: controller.signal, redirect: 'manual' }), timeout, controller.signal, () => controller.abort());
        if (response.status < 200 || response.status >= 300) { void response.body?.cancel().catch(() => {}); throw new Error(`MCP HTTP refused request: ${response.status}.`); }
        if (response.status === 202 || response.status === 204) { void response.body?.cancel().catch(() => {}); return; }
        if (!response.headers.get('content-type')?.startsWith('application/json')) { void response.body?.cancel().catch(() => {}); throw new Error('MCP client supports JSON HTTP responses, not SSE.'); }
        const value: unknown = JSON.parse(await readBody(response.body, 1024 * 1024, timeout, controller.signal));
        if (object(message) && message.method === 'initialize' && object(value) && object(value.result)) {
          session = response.headers.get('mcp-session-id') ?? undefined;
          if (typeof value.result.protocolVersion === 'string') revision = value.result.protocolVersion;
        }
        receive(value);
      } finally { controllers.delete(controller); if (id !== undefined) active.delete(id); }
    },
    async close() {
      if (closed) return;
      closed = true;
      for (const controller of controllers) controller.abort();
      if (!session) return;
      const controller = new AbortController();
      try {
        const response = await bounded(request(url, { method: 'DELETE', headers: headers(), timeout, signal: controller.signal, redirect: 'manual' }), timeout, undefined, () => controller.abort());
        void response.body?.cancel().catch(() => {});
      } finally { controller.abort(); }
    },
  };
}
