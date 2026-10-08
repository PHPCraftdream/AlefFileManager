// SPDX-License-Identifier: MIT OR Apache-2.0
import { Peer, RpcError, errors } from './core.ts';
import { validate, type Schema } from './schema.ts';
import { negotiate, object, type Context, type Handler, type Identity, type ObjectValue, type Transport } from './types.ts';
import { stdio, type Streams } from './transports/stdio.ts';
import { consoleStreams, runtime } from './transports/runtime.ts';
import { serveHttp, type HttpOptions, type HttpListener } from './transports/http-server.ts';
export interface ToolOptions { description?: string; inputSchema: Schema }
export interface ResourceOptions { name?: string; description?: string; mimeType?: string }
export interface PromptOptions { description?: string; arguments?: Array<{ name: string; description?: string; required?: boolean }> }
export type ListenOptions = { transport: Transport } | { stdio: true | Streams } | { http: HttpOptions };
interface Entry<T> { options: T; handler: Handler }
export class McpServer {
  readonly info: Identity;
  token?: string;
  #tools = new Map<string, Entry<ToolOptions>>();
  #resources = new Map<string, Entry<ResourceOptions>>();
  #prompts = new Map<string, Entry<PromptOptions>>();
  #peers = new Set<Peer>();
  #listener?: HttpListener;
  #closed = false;
  #listening = false;
  #single = false;
  #ended: () => void = () => {};
  /** Settles when the server is closed, or when the only connection of a stdio or transport server ends. */
  readonly closed: Promise<void> = new Promise(resolve => { this.#ended = resolve; });
  constructor(info: Identity) { this.info = info; }
  get url(): string | undefined { return this.#listener?.url; }
  get address(): { host: string; port: number } | undefined { return this.#listener?.address; }
  tool(name: string, options: ToolOptions, handler: Handler): this {
    this.#tools.set(name, { options, handler }); return this;
  }
  resource(uri: string, options: ResourceOptions, handler: Handler): this {
    this.#resources.set(uri, { options, handler }); return this;
  }
  prompt(name: string, options: PromptOptions, handler: Handler): this {
    this.#prompts.set(name, { options, handler }); return this;
  }
  /** Attaches a single protocol session; also useful for in-memory transports. */
  attach(transport: Transport): Peer {
    if (this.#closed) throw new Error('MCP server is closed.');
    const peer = new Peer({
      send: message => transport.send(message),
      cancel: id => transport.cancel?.(id),
      start: (receive, end) => transport.start(receive, error => { queueMicrotask(() => this.#peers.delete(peer)); if (this.#single) this.#ended(); end(error); }),
      close: async () => { this.#peers.delete(peer); await transport.close(); },
    });
    let phase: 'new' | 'initializing' | 'ready' = 'new';
    peer.handler = async (method, params, context) => {
      if (!object(params)) throw new RpcError(errors.invalidParams, 'MCP parameters must be an object.');
      if (method === 'initialize') {
        if (phase !== 'new') throw new RpcError(errors.invalidRequest, 'Session is already initialized.');
        if (typeof params.protocolVersion !== 'string' || !object(params.capabilities) || !object(params.clientInfo) || typeof params.clientInfo.name !== 'string' || typeof params.clientInfo.version !== 'string') throw new RpcError(errors.invalidParams, 'Invalid initialize parameters.');
        peer.revision = negotiate(params.protocolVersion); phase = 'initializing';
        return { protocolVersion: peer.revision, capabilities: { tools: {}, resources: {}, prompts: {} }, serverInfo: this.info };
      }
      if (method === 'notifications/initialized') {
        if (phase === 'initializing') phase = 'ready';
        return {};
      }
      if (method === 'ping') return {};
      if (phase !== 'ready') throw new RpcError(errors.invalidRequest, 'Session is not initialized.');
      return this.#handle(method, params, context);
    };
    this.#peers.add(peer);
    return peer;
  }
  async #handle(method: string, params: ObjectValue, context: Context): Promise<unknown> {
    if (method.endsWith('/list') && params.cursor !== undefined) throw new RpcError(errors.invalidParams, 'Pagination cursors are not supported.');
    switch (method) {
      case 'tools/list': return { tools: [...this.#tools].map(([name, entry]) => ({ name, ...entry.options })) };
      case 'resources/list': return { resources: [...this.#resources].map(([uri, entry]) => ({ uri, name: entry.options.name ?? uri, ...entry.options })) };
      case 'prompts/list': return { prompts: [...this.#prompts].map(([name, entry]) => ({ name, ...entry.options })) };
      case 'tools/call': {
        const entry = typeof params.name === 'string' ? this.#tools.get(params.name) : undefined;
        if (!entry) throw new RpcError(errors.invalidParams, 'Unknown tool.');
        const args = params.arguments ?? {};
        if (!object(args)) throw new RpcError(errors.invalidParams, 'Tool arguments must be an object.');
        const issues = validate(entry.options.inputSchema, args);
        if (issues.length) throw new RpcError(errors.invalidParams, 'Tool arguments do not match inputSchema.', issues);
        return entry.handler(args, context);
      }
      case 'resources/read': {
        const entry = typeof params.uri === 'string' ? this.#resources.get(params.uri) : undefined;
        if (!entry) throw new RpcError(errors.invalidParams, 'Unknown resource.');
        return entry.handler(params, context);
      }
      case 'prompts/get': {
        const entry = typeof params.name === 'string' ? this.#prompts.get(params.name) : undefined;
        if (!entry) throw new RpcError(errors.invalidParams, 'Unknown prompt.');
        const args = params.arguments ?? {};
        if (!object(args) || Object.values(args).some(value => typeof value !== 'string')) throw new RpcError(errors.invalidParams, 'Prompt arguments must be strings.');
        for (const arg of entry.options.arguments ?? []) if (arg.required && !Object.hasOwn(args, arg.name)) throw new RpcError(errors.invalidParams, `Missing prompt argument: ${arg.name}.`);
        return entry.handler(args, context);
      }
      default:
        if (method.startsWith('notifications/')) return {};
        throw new RpcError(errors.methodNotFound, 'Method not found.');
    }
  }
  async listen(options: ListenOptions): Promise<this> {
    if (this.#closed || this.#listening) throw new Error('MCP server is closed or already listening.');
    this.#listening = true;
    try {
      if ('transport' in options) { this.#single = true; this.attach(options.transport); }
      else if ('stdio' in options) { this.#single = true; this.attach(stdio(options.stdio === true ? consoleStreams() : options.stdio)); }
      else {
        this.#listener = await serveHttp(options.http, transport => this.attach(transport), runtime.http.serve);
        this.token = this.#listener.token;
      }
      return this;
    } catch (error) { this.#listening = false; throw error; }
  }
  async close(): Promise<void> {
    if (this.#closed) return;
    this.#closed = true;
    await Promise.allSettled([...this.#peers].map(peer => peer.close()));
    this.#peers.clear();
    await this.#listener?.close();
    this.#ended();
  }
}
