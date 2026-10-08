// SPDX-License-Identifier: MIT OR Apache-2.0
import { duration, object, type Context, type Handler, type ObjectValue, type RequestOptions, type Revision, type Transport } from './types.ts';
export class RpcError extends Error {
  readonly code: number;
  readonly data?: unknown;
  constructor(code: number, message: string, data?: unknown) {
    super(message); this.name = 'RpcError'; this.code = code; this.data = data;
  }
}
export const errors = { parse: -32700, invalidRequest: -32600, methodNotFound: -32601, invalidParams: -32602, internal: -32603 } as const;
const failure = (id: unknown, error: RpcError) => ({ jsonrpc: '2.0', id, error: { code: error.code, message: error.message, ...(error.data === undefined ? {} : { data: error.data }) } });
const validId = (id: unknown): id is string | number => typeof id === 'string' || (typeof id === 'number' && Number.isFinite(id));
type RpcParams = ObjectValue | unknown[];
const requestId = (id: unknown): id is string | number | null => id === null || validId(id);
interface Pending { resolve(value: unknown): void; reject(error: unknown): void; clean(): void; progress?: RequestOptions['onProgress'] }
export class Peer {
  // March batching remains a generic core compatibility mode, never negotiated by MCP.
  revision: Revision | '2025-03-26' = '2025-11-25';
  handler?: (method: string, params: RpcParams, context: Context) => unknown | Promise<unknown>;
  #transport: Transport;
  #pending = new Map<string | number, Pending>();
  #active = new Map<string | number | null, { controller: AbortController; method: string }>();
  #next = 0;
  #closed = false;
  constructor(transport: Transport) {
    this.#transport = transport;
    transport.start(message => { void this.dispatch(message).then(reply => reply === undefined ? undefined : transport.send(reply)).catch(error => this.end(error)); }, error => this.end(error));
  }
  end(error: unknown = new Error('MCP connection closed.')): void {
    if (this.#closed) return;
    this.#closed = true;
    for (const pending of this.#pending.values()) { pending.clean(); pending.reject(error); }
    this.#pending.clear();
    for (const { controller } of this.#active.values()) controller.abort(error);
    this.#active.clear();
  }
  async close(): Promise<void> { this.end(); await this.#transport.close(); }
  async notify(method: string, params?: RpcParams): Promise<void> {
    if (this.#closed) throw new Error('MCP connection closed.');
    await this.#transport.send({ jsonrpc: '2.0', method, ...(params === undefined ? {} : { params }) });
  }
  request(method: string, params: RpcParams = {}, options: RequestOptions = {}): Promise<unknown> {
    if (this.#closed) return Promise.reject(new Error('MCP connection closed.'));
    const timeout = duration(options.timeout);
    if (options.signal?.aborted) return Promise.reject(options.signal.reason ?? new Error('Request cancelled.'));
    const id = ++this.#next;
    return new Promise((resolve, reject) => {
      const stop = (error: unknown) => {
        const pending = this.#pending.get(id);
        if (!pending) return;
        this.#pending.delete(id); pending.clean(); reject(error);
        this.#transport.cancel?.(id);
        if (method !== 'initialize') void this.notify('notifications/cancelled', { requestId: id, reason: 'Request cancelled or timed out.' }).catch(() => {});
      };
      const abort = () => stop(options.signal?.reason ?? new Error('Request cancelled.'));
      const timer = setTimeout(() => stop(new Error('MCP request timed out.')), timeout);
      this.#pending.set(id, { resolve, reject, progress: options.onProgress, clean: () => { clearTimeout(timer); options.signal?.removeEventListener('abort', abort); } });
      options.signal?.addEventListener('abort', abort, { once: true });
      const input = options.onProgress && object(params) ? { ...params, _meta: { ...(object(params['_meta']) ? params['_meta'] : {}), progressToken: id } } : params;
      void this.#transport.send({ jsonrpc: '2.0', id, method, params: input }).catch(stop);
    });
  }
  async dispatch(message: unknown, signal?: AbortSignal): Promise<unknown | undefined> {
    if (Array.isArray(message)) {
      if (this.revision !== '2025-03-26' || message.length === 0) return failure(null, new RpcError(errors.invalidRequest, 'Batches are not allowed.'));
      const results = await Promise.all(message.map(item => Array.isArray(item) ? failure(null, new RpcError(errors.invalidRequest, 'Nested batches are not allowed.')) : this.dispatch(item, signal)));
      const replies = results.filter(item => item !== undefined);
      return replies.length ? replies : undefined;
    }
    if (!object(message) || message.jsonrpc !== '2.0') return failure(null, new RpcError(errors.invalidRequest, 'Invalid JSON-RPC request.'));
    if (typeof message.method !== 'string') {
      if (message.id === null && (Object.hasOwn(message, 'result') !== Object.hasOwn(message, 'error'))) return;
      if (validId(message.id) && (Object.hasOwn(message, 'result') !== Object.hasOwn(message, 'error'))) {
        const pending = this.#pending.get(message.id);
        if (!pending) return;
        this.#pending.delete(message.id); pending.clean();
        if (object(message.error) && typeof message.error.code === 'number' && typeof message.error.message === 'string') pending.reject(new RpcError(message.error.code, message.error.message, message.error.data));
        else if (Object.hasOwn(message, 'result')) pending.resolve(message.result);
        else pending.reject(new RpcError(errors.invalidRequest, 'Invalid response.'));
        return;
      }
      return failure(null, new RpcError(errors.invalidRequest, 'Invalid JSON-RPC message.'));
    }
    const hasId = Object.hasOwn(message, 'id');
    if (hasId && !requestId(message.id)) return failure(null, new RpcError(errors.invalidRequest, 'Invalid request id.'));
    if (message.params !== undefined && !object(message.params) && !Array.isArray(message.params)) return hasId ? failure(message.id, new RpcError(errors.invalidParams, 'Parameters must be an object or array.')) : undefined;
    const params = (message.params ?? {}) as RpcParams;
    if (message.method.startsWith('notifications/') && hasId) return failure(message.id, new RpcError(errors.invalidRequest, 'Notification methods must not carry an id.'));
    if (message.method === 'notifications/cancelled') {
      if (object(params) && validId(params.requestId)) {
        const active = this.#active.get(params.requestId);
        if (active?.method !== 'initialize') active?.controller.abort(new Error('Peer cancelled request.'));
      }
      return;
    }
    if (message.method === 'notifications/progress') {
      if (object(params) && validId(params.progressToken)) {
        try { this.#pending.get(params.progressToken)?.progress?.(params); } catch { /* User callbacks cannot corrupt the connection. */ }
      }
      return;
    }
    const id = message.id as string | number | null;
    if (hasId && this.#active.has(id)) return failure(id, new RpcError(errors.invalidRequest, 'Duplicate active request id.'));
    const controller = new AbortController();
    if (hasId) this.#active.set(id, { controller, method: message.method });
    const abort = () => controller.abort(signal?.reason);
    signal?.addEventListener('abort', abort, { once: true });
    if (signal?.aborted) abort();
    const token = object(params) && object(params['_meta']) ? params['_meta'].progressToken : undefined;
    const context: Context = { signal: controller.signal, progress: async (progress, total, text) => {
      if (validId(token) && !controller.signal.aborted) await this.notify('notifications/progress', { progressToken: token, progress, ...(total === undefined ? {} : { total }), ...(text === undefined ? {} : { message: text }) });
    } };
    let stop: (() => void) | undefined;
    try {
      if (!this.handler) throw new RpcError(errors.methodNotFound, 'Method not found.');
      const cancelled = new Promise<never>((_resolve, reject) => {
        stop = () => reject(controller.signal.reason ?? new Error('Request cancelled.'));
        controller.signal.addEventListener('abort', stop, { once: true });
      });
      if (controller.signal.aborted) throw controller.signal.reason ?? new Error('Request cancelled.');
      const result = await Promise.race([Promise.resolve(this.handler(message.method, params, context)), cancelled]);
      return hasId ? { jsonrpc: '2.0', id, result: result ?? {} } : undefined;
    } catch (error) {
      return hasId ? failure(id, error instanceof RpcError ? error : new RpcError(errors.internal, 'Internal error.')) : undefined;
    } finally {
      signal?.removeEventListener('abort', abort);
      if (stop) controller.signal.removeEventListener('abort', stop);
      if (hasId) this.#active.delete(id);
    }
  }
}
export function parse(text: string): unknown {
  try { return JSON.parse(text); } catch { throw new RpcError(errors.parse, 'Parse error.'); }
}
export type { Handler };
