// SPDX-License-Identifier: MIT OR Apache-2.0
import { AlefError } from '../core/errors.ts';
import { call } from '../core/transport.ts';
import type { Cancelable } from '../desktop/app.ts';
import { http } from './http.ts';
import type { HttpServer, ServeOptions, ServerRequest } from './http-server.ts';
import { type Opened, WebSocketConnection } from './websocket-connection.ts';

export { WebSocketConnection, type CloseInfo, type WebSocketMessage } from './websocket-connection.ts';

export interface WebSocketOptions extends Cancelable {
  /** Subprotocols to offer, in order of preference; the server chooses one (`protocol`). */
  protocols?: string[];
  /** Headers of the handshake (`Origin`, `Authorization`...); the ones of the handshake itself are the runtime's. */
  headers?: HeadersInit;
  /** PEM certificates of the authorities to trust for `wss://` instead of the roots of Mozilla. */
  ca?: string;
  /** Milliseconds the connection and the handshake have; it ends with `TIMEOUT` after that. */
  timeout?: number;
}

export interface WebSocketServeOptions extends Pick<ServeOptions, 'host' | 'port' | 'tls' | 'hosts' | 'origins' | 'signal'> {
  /** The path the server takes WebSockets on; a client that asks for another is answered with a 404. Any path when omitted. */
  path?: string;
  /** The subprotocols the server speaks, in order of preference: the first one the client offers is chosen. A client that offers some and none of these is answered with a 400. */
  protocols?: string[];
}

/**
 * A server of WebSocket: iterate it once for the connections it takes, each already open. What is no offer of a
 * WebSocket is answered with a 426, and the connections stay open when the server is closed.
 */
export class WebSocketServer implements AsyncIterable<WebSocketConnection> {
  readonly address: { host: string; port: number };
  readonly secure: boolean;
  #server: HttpServer;
  #path: string | undefined;
  #protocols: string[];

  constructor(server: HttpServer, options: { path?: string; protocols?: string[] }) {
    this.#server = server;
    this.#path = options.path;
    this.#protocols = options.protocols ?? [];
    this.address = server.address;
    this.secure = server.secure;
  }

  /** Where clients come: `ws://127.0.0.1:port`, or `wss://` for a server with TLS, with the path if there is one. */
  get url(): string {
    return `${this.#server.url.replace(/^http/, 'ws')}${this.#path ?? ''}`;
  }

  async *[Symbol.asyncIterator](): AsyncGenerator<WebSocketConnection> {
    for await (const request of this.#server) {
      const connection = await this.#take(request);
      if (connection !== null) yield connection;
    }
  }

  /** Stops taking connections; the ones it gave stay open. Safe to call repeatedly. */
  close(): Promise<void> {
    return this.#server.close();
  }

  /** The connection a request makes, or `null` when it was refused (or the client went away). */
  async #take(request: ServerRequest): Promise<WebSocketConnection | null> {
    const refuse = async (status: number, body: string, headers?: HeadersInit): Promise<null> => {
      await request.respond({ status, headers, body });
      return null;
    };
    try {
      if (this.#path !== undefined && new URL(request.url, 'http://server').pathname !== this.#path) return await refuse(404, 'No WebSocket here.');
      if (!request.upgradable) return await refuse(426, 'This server speaks WebSocket.', { 'sec-websocket-version': '13' });
      let protocol: string | undefined;
      if (this.#protocols.length > 0 && request.protocols.length > 0) {
        protocol = this.#protocols.find(wanted => request.protocols.includes(wanted));
        if (protocol === undefined) return await refuse(400, 'No subprotocol in common.');
      }
      return await request.upgrade({ protocol });
    } catch {
      return null;
    }
  }
}

/**
 * Connections to servers of WebSocket (RFC 6455), held against `permissions.net.http` (patterns like
 * `wss://chat.example.com/*`). `serve` takes a port of this machine instead (`permissions.net.socket`:
 * `listen:127.0.0.1:*`); a server of HTTP takes the same offers with `ServerRequest.upgrade`.
 */
export const websocket = {
  connect: async (url: string, options: WebSocketOptions = {}): Promise<WebSocketConnection> => {
    const { protocols, headers, ca, timeout, signal } = options;
    const pairs = headers === undefined ? undefined : [...new Headers(headers)];
    return new WebSocketConnection(await call<Opened>('websocket.connect', { url, protocols, headers: pairs, ca, timeoutMs: timeout }, { signal }));
  },

  serve: async (options: WebSocketServeOptions = {}): Promise<WebSocketServer> => {
    const { path, protocols, ...listen } = options;
    if (path !== undefined && !path.startsWith('/')) throw new AlefError('INVALID_ARGUMENT', 'A path starts with a slash.');
    return new WebSocketServer(await http.serve(listen), { path, protocols });
  },
};
