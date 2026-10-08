// SPDX-License-Identifier: MIT OR Apache-2.0
import type { ServeOptions, ServerResponse } from '../../../api/src/net/http-server.ts';
import { Peer, parse } from '../core.ts';
import { duration, object, revisions, type Transport } from '../types.ts';
import { runtime } from './runtime.ts';
import { bounded, readBody } from './body.ts';
export interface HttpOptions {
  host?: string; port?: number; path?: string; token?: string | false;
  checkHost?: boolean; checkOrigin?: boolean; hosts?: string[]; allowedOrigins?: string[]; origins?: string[];
  maxBodyBytes?: number; timeout?: number; maxSessions?: number;
}
export interface Request {
  method: string; url: string; headers: Headers; body: ReadableStream<Uint8Array> | null;
  respond(response: ServerResponse): Promise<void>;
}
export interface NativeServer extends AsyncIterable<Request> {
  address: { host: string; port: number }; url: string; close(): Promise<void>;
}
export interface HttpListener { address: NativeServer['address']; url: string; token?: string; close(): Promise<void> }
/** Runtime http.serve owns document lifetime and closes native connections on document teardown. */
export async function serveHttp(options: HttpOptions, attach: (transport: Transport) => Peer, serve: (options: ServeOptions) => Promise<NativeServer>): Promise<HttpListener> {
  const host = options.host ?? '127.0.0.1';
  if (!['127.0.0.1', '::1', 'localhost'].includes(host)) throw new Error('MCP HTTP supports loopback binding only.');
  const path = options.path ?? '/mcp';
  if (!path.startsWith('/') || path.includes('?') || path.includes('#')) throw new Error('MCP path must be an absolute URL path.');
  const timeout = duration(options.timeout);
  const limit = options.maxBodyBytes ?? 1024 * 1024;
  const maxSessions = options.maxSessions ?? 128;
  if (!Number.isInteger(limit) || limit <= 0 || !Number.isInteger(maxSessions) || maxSessions <= 0) throw new Error('Body and session limits must be positive integers.');
  const randomToken = async () => {
    const bytes = await bounded(runtime.crypto.random(32), timeout);
    if (bytes.length !== 32) throw new Error('Secure random source returned an invalid token length.');
    return Array.from(bytes, byte => byte.toString(16).padStart(2, '0')).join('');
  };
  const token = options.token === false ? undefined : options.token ?? await randomToken();
  if (token !== undefined && (!token || /\s/.test(token))) throw new Error('Token must be a nonempty token without whitespace.');
  const origins = options.allowedOrigins ?? options.origins;
  const native = await serve({ host, port: options.port, hosts: options.hosts, origins, answerTimeout: timeout });
  const sessions = new Map<string, Peer>();
  const controller = new AbortController();
  let closed = false;
  const handle = async (request: Request) => {
    const answer = (status: number, body?: unknown, headers: Record<string, string> = {}) => request.respond({ status, headers: { ...(body === undefined ? {} : { 'content-type': 'application/json' }), ...headers }, ...(body === undefined ? {} : { body: JSON.stringify(body) }) });
    const refuse = async (status: number) => { void request.body?.cancel().catch(() => {}); await answer(status); };
    const authority = request.headers.get('host');
    const port = native.address.port;
    const allowedHosts = [`127.0.0.1:${port}`, `localhost:${port}`, `[::1]:${port}`, ...(options.hosts ?? [])];
    if (options.checkHost !== false && (!authority || !allowedHosts.includes(authority.toLowerCase()))) return refuse(authority ? 421 : 400);
    const origin = request.headers.get('origin');
    if (options.checkOrigin !== false && origin !== null) {
      let allowed = (origins ?? []).includes(origin);
      try {
        const parsed = new URL(origin);
        allowed ||= parsed.origin === origin && parsed.protocol === 'http:' && allowedHosts.slice(0, 3).includes(parsed.host);
      } catch { /* Reject malformed origins. */ }
      if (!allowed) return refuse(403);
    }
    if (token !== undefined && request.headers.get('authorization') !== `Bearer ${token}`) return refuse(401);
    if (request.url.split('?')[0] !== path) return refuse(404);
    if (request.method === 'GET') return refuse(405);
    if (!['POST', 'DELETE'].includes(request.method)) return refuse(405);
    const sessionId = request.headers.get('mcp-session-id');
    const peer = sessionId ? sessions.get(sessionId) : undefined;
    if (sessionId && !peer) return refuse(404);
    const version = request.headers.get('mcp-protocol-version');
    if (version !== null && !revisions.some(revision => revision === version)) return refuse(400);
    // Missing header is interpreted as March, per the HTTP protocol compatibility rule.
    if (peer && (version ?? '2025-03-26') !== peer.revision) return refuse(400);
    if (request.method === 'DELETE') {
      if (!peer || !sessionId) return refuse(400);
      sessions.delete(sessionId); await peer.close(); return refuse(204);
    }
    if (!request.headers.get('content-type')?.toLowerCase().startsWith('application/json')) return refuse(415);
    const accept = request.headers.get('accept') ?? '';
    if (!accept.includes('application/json') || !accept.includes('text/event-stream')) return refuse(406);
    let message: unknown;
    try { message = parse(await readBody(request.body, limit, timeout, controller.signal)); }
    catch (error) {
      const oversized = error instanceof Error && error.message === 'Body too large.';
      return answer(oversized ? 413 : 400, { jsonrpc: '2.0', id: null, error: { code: -32700, message: oversized ? 'Body too large.' : 'Parse error.' } });
    }
    let current = peer;
    let id = sessionId;
    let fresh = false;
    if (!current) {
      if (!object(message) || message.method !== 'initialize' || !Object.hasOwn(message, 'id')) return answer(400);
      if (sessions.size >= maxSessions) return answer(503);
      current = attach({ send: async () => {}, start: () => {}, close: async () => {} });
      id = await randomToken(); fresh = true;
    }
    const dispatchController = new AbortController();
    const response = await bounded(current.dispatch(message, dispatchController.signal), timeout, controller.signal, () => dispatchController.abort(new Error('HTTP dispatch timed out or closed.'))).catch(() => ({ jsonrpc: '2.0', id: object(message) ? message.id ?? null : null, error: { code: -32603, message: 'Request timed out.' } }));
    if (fresh) {
      if (!object(response) || !Object.hasOwn(response, 'result')) { await current.close(); return answer(400, response); }
      sessions.set(id!, current);
    }
    await answer(response === undefined ? 202 : 200, response, { 'mcp-session-id': id! });
  };
  void (async () => {
    try { for await (const request of native) void handle(request).catch(() => {}); }
    finally { controller.abort(); for (const peer of sessions.values()) peer.end(); sessions.clear(); }
  })().catch(() => {});
  return { address: native.address, url: `${native.url}${path}`, token, async close() {
    if (closed) return;
    closed = true; controller.abort();
    await Promise.allSettled([...sessions.values()].map(peer => peer.close())); sessions.clear(); await native.close();
  } };
}
