// SPDX-License-Identifier: MIT OR Apache-2.0
import { Peer } from './core.ts';
import { duration, object, revisions, type Identity, type ObjectValue, type RequestOptions, type Transport } from './types.ts';
import { runtime } from './transports/runtime.ts';
import { stdio } from './transports/stdio.ts';
import { httpTransport } from './transports/http-client.ts';
import { bounded } from './transports/body.ts';
export type ConnectOptions = ({ transport: Transport } | { command: string; args?: string[] } | { url: string; token?: string }) & RequestOptions & { clientInfo?: Identity; protocolVersion?: string };
export class McpClient {
  readonly peer: Peer;
  readonly serverInfo: ObjectValue;
  readonly capabilities: ObjectValue;
  #timeout: number;
  constructor(peer: Peer, result: ObjectValue, timeout: number) {
    this.peer = peer; this.serverInfo = result.serverInfo as ObjectValue; this.capabilities = result.capabilities as ObjectValue; this.#timeout = timeout;
  }
  #request(method: string, params: ObjectValue, options: RequestOptions = {}): Promise<unknown> { return this.peer.request(method, params, { timeout: this.#timeout, ...options }); }
  listTools(options?: RequestOptions): Promise<unknown> { return this.#request('tools/list', {}, options); }
  callTool(name: string, args: ObjectValue = {}, options?: RequestOptions): Promise<unknown> { return this.#request('tools/call', { name, arguments: args }, options); }
  listResources(options?: RequestOptions): Promise<unknown> { return this.#request('resources/list', {}, options); }
  readResource(uri: string, options?: RequestOptions): Promise<unknown> { return this.#request('resources/read', { uri }, options); }
  listPrompts(options?: RequestOptions): Promise<unknown> { return this.#request('prompts/list', {}, options); }
  getPrompt(name: string, args: Record<string, string> = {}, options?: RequestOptions): Promise<unknown> { return this.#request('prompts/get', { name, arguments: args }, options); }
  async close(): Promise<void> { await this.peer.close(); }
}
export async function connect(options: ConnectOptions): Promise<McpClient> {
  const timeout = duration(options.timeout);
  options.signal?.throwIfAborted();
  let transport: Transport;
  if ('transport' in options) transport = options.transport;
  else if ('url' in options) transport = httpTransport(options.url, options.token, timeout, runtime.http.request);
  else {
    const controller = new AbortController();
    const spawning = runtime.cli.spawn(options.command, options.args ?? [], { stdin: 'pipe', stdout: 'pipe', stderr: 'ignore', signal: controller.signal });
    let expired = false;
    void spawning.then(child => { if (expired) void child.kill().catch(() => {}); }).catch(() => {});
    const child = await bounded(spawning, timeout, options.signal, () => { expired = true; controller.abort(); });
    if (!child.stdout || !child.stdin) { await child.kill(); throw new Error('MCP child process requires stdin and stdout pipes.'); }
    const stream = stdio({ readable: child.stdout, writable: child.stdin });
    transport = { send: message => stream.send(message), start: (receive, end) => stream.start(receive, end), close: async () => {
      await stream.close();
      await bounded(child.kill(), timeout).catch(() => {});
      await bounded(child.wait(), timeout).catch(() => {});
    } };
  }
  const peer = new Peer(transport);
  try {
    const result = await peer.request('initialize', { protocolVersion: options.protocolVersion ?? revisions[0], capabilities: {}, clientInfo: options.clientInfo ?? { name: '@alef-tron/mcp', version: '0.1.0' } }, options);
    if (!object(result) || !revisions.some(version => version === result.protocolVersion) || !object(result.serverInfo) || !object(result.capabilities)) throw new Error('Server returned an unsupported initialization response.');
    peer.revision = result.protocolVersion as typeof peer.revision;
    await bounded(peer.notify('notifications/initialized'), timeout, options.signal);
    return new McpClient(peer, result, timeout);
  } catch (error) { await bounded(peer.close(), timeout).catch(() => {}); throw error; }
}
