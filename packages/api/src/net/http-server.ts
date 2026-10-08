// SPDX-License-Identifier: MIT OR Apache-2.0
import { AlefError } from '../core/errors.ts';
import { openReadable } from '../core/stream.ts';
import { call } from '../core/transport.ts';
import { bytesOf } from '../core/web-stream.ts';
import type { Cancelable } from '../desktop/app.ts';
import { type Body, collect, pairs, UNARY_BODY, upload } from './body.ts';
import { type Opened, WebSocketConnection } from './websocket-connection.ts';

const encoder = new TextEncoder();
const decoder = new TextDecoder();

export interface ServeOptions extends Cancelable {
  /** The address to listen on; this machine alone (`127.0.0.1`) when omitted. */
  host?: string;
  /** `0` or omitted: any free port, which `address` tells. */
  port?: number;
  /** PEM certificate chain and private key: the server speaks HTTPS and nothing else. */
  tls?: { cert: string; key: string };
  /** A folder (it needs `permissions.fs.read`) whose files the server gives by itself, `GET` and `HEAD` alone; what is not one of them comes to the page. */
  files?: string;
  /** Names besides the ones of this machine that a `Host` may carry. */
  hosts?: string[];
  /** Origins besides the ones of this server that a request may come from. */
  origins?: string[];
  /** Milliseconds the page has to answer a request, before the client gets a 504; 60 000 when omitted. */
  answerTimeout?: number;
}

export interface ServerResponse {
  /** 200 to 599; 200 when omitted. */
  status?: number;
  headers?: HeadersInit;
  /** A text, bytes, or a stream that goes down as it is read; none when omitted. */
  body?: Body | null;
}

interface ServerFrame {
  id: number;
  method: string;
  url: string;
  headers: Array<[string, string]>;
  body: number | null;
  upgrade?: boolean;
  protocols?: string[];
}

export interface UpgradeOptions {
  /** The subprotocol to speak: one the client offered (`protocols`); none when omitted. */
  protocol?: string;
}

/** A request to the server of the page. It is answered once, with `respond`. */
export class ServerRequest {
  readonly method: string;
  /** The path and the query, as the client wrote them. */
  readonly url: string;
  readonly headers: Headers;
  /** Whether the client offers a WebSocket (a `GET` that asks for the upgrade, with a key and the version 13): `upgrade` takes the offer. */
  readonly upgradable: boolean;
  /** The subprotocols the client offers, in the order it prefers them; empty for a request that is no offer. */
  readonly protocols: readonly string[];
  #id: number;
  #stream: number | null;
  #used = false;
  #answered = false;

  constructor(frame: ServerFrame) {
    this.#id = frame.id;
    this.method = frame.method;
    this.url = frame.url;
    this.headers = new Headers();
    for (const [name, value] of frame.headers) this.headers.append(name, value);
    this.#stream = frame.body;
    this.upgradable = frame.upgrade === true;
    this.protocols = frame.protocols ?? [];
  }

  get bodyUsed(): boolean {
    return this.#used;
  }

  /** The body as a stream with backpressure; `null` when the request has none. */
  get body(): ReadableStream<Uint8Array> | null {
    if (this.#stream === null || this.#used) return null;
    this.#used = true;
    return bytesOf(this.#stream);
  }

  /** The whole body. */
  async bytes(): Promise<Uint8Array<ArrayBuffer>> {
    if (this.#used) throw new AlefError('INVALID_ARGUMENT', 'The body was read already.');
    return collect(this.body);
  }

  async text(): Promise<string> {
    return decoder.decode(await this.bytes());
  }

  async json(): Promise<unknown> {
    return JSON.parse(await this.text());
  }

  /**
   * Takes the offer of a WebSocket: the client is answered with a 101, and the connection is the page's.
   * A request that is no offer, or a subprotocol the client did not offer, is `INVALID_ARGUMENT` and leaves the
   * request to be answered otherwise. The connection stays open when the server is closed.
   */
  async upgrade(options: UpgradeOptions = {}): Promise<WebSocketConnection> {
    if (this.#answered) throw new AlefError('INVALID_ARGUMENT', 'The request was answered already.');
    if (!this.upgradable) throw new AlefError('INVALID_ARGUMENT', 'The request does not offer a WebSocket.');
    const opened = await call<Omit<Opened, 'url'>>('http.upgrade', { request: this.#id, protocol: options.protocol });
    this.#answered = true;
    return new WebSocketConnection({ ...opened, url: this.url });
  }

  /**
   * Answers the request. A request the client gave up on is `NOT_FOUND`; a request is answered once.
   */
  async respond(response: ServerResponse = {}): Promise<void> {
    if (this.#answered) throw new AlefError('INVALID_ARGUMENT', 'The request was answered already.');
    const { status, headers, body } = response;
    const args = { request: this.#id, status, headers: pairs(headers) };
    const small = typeof body === 'string' ? encoder.encode(body) : body instanceof Uint8Array ? body : undefined;
    if (body === undefined || body === null || (small !== undefined && small.length <= UNARY_BODY)) {
      await call<null>('http.respond', args, { body: small && small.length > 0 ? small : undefined });
      this.#answered = true;
      return;
    }
    const { upload: stream } = await call<{ upload: number }>('http.respondStream', args);
    this.#answered = true;
    await upload(stream, body, {});
  }
}

/**
 * A server of HTTP: iterate it once for the requests, answer each with `respond` in any order. The
 * iteration ends when the server is closed.
 */
export class HttpServer implements AsyncIterable<ServerRequest> {
  readonly address: { host: string; port: number };
  readonly secure: boolean;
  #id: number;
  #requests: number;
  #closed = false;

  constructor(opened: { server: number; requests: number; address: { host: string; port: number }; secure: boolean }) {
    this.#id = opened.server;
    this.#requests = opened.requests;
    this.address = opened.address;
    this.secure = opened.secure;
  }

  /** The address clients use: `http://127.0.0.1:port`, or `https://` for a server with TLS. */
  get url(): string {
    const host = this.address.host.includes(':') ? `[${this.address.host}]` : this.address.host;
    return `${this.secure ? 'https' : 'http'}://${host}:${this.address.port}`;
  }

  async *[Symbol.asyncIterator](): AsyncGenerator<ServerRequest> {
    try {
      for await (const frame of await openReadable(this.#requests)) {
        if (frame.kind === 'json') yield new ServerRequest(frame.value as ServerFrame);
      }
    } catch (error) {
      if (!this.#closed) throw error;
    }
  }

  /** Stops taking connections and requests; the requests it had are dropped. Safe to call repeatedly. */
  async close(): Promise<void> {
    if (this.#closed) return;
    this.#closed = true;
    await call<null>('socket.close', { socket: this.#id });
  }
}
