// SPDX-License-Identifier: MIT OR Apache-2.0
import { AlefError } from '../core/errors.ts';
import { openReadable } from '../core/stream.ts';
import { call } from '../core/transport.ts';
import type { Cancelable } from '../desktop/app.ts';

const encoder = new TextEncoder();
const decoder = new TextDecoder();

/** A message the page sends travels in the body of a call: at most this many bytes. */
const MAX_SEND = 192 * 1024;

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

export type WebSocketMessage = { type: 'text'; data: string } | { type: 'binary'; data: Uint8Array<ArrayBuffer> };

export interface CloseInfo {
  /** 1005 when the peer sent no code, 1006 when the connection ended without a close. */
  code: number;
  reason: string;
  /** Whether the closing handshake was done. */
  clean: boolean;
}

interface Opened {
  socket: number;
  messages: number;
  protocol: string;
  url: string;
}

/**
 * A connection to a server of WebSocket: iterate it once for the messages it brings. The iteration ends
 * with the connection, and leaving it early (a `break`) closes the connection.
 */
export class WebSocketConnection implements AsyncIterable<WebSocketMessage> {
  readonly url: string;
  /** The subprotocol the server chose, or an empty string. */
  readonly protocol: string;
  /** Resolves with how the connection ended, once the iteration saw it end (it never rejects). */
  readonly closed: Promise<CloseInfo>;
  #id: number;
  #messages: number;
  #done = false;
  #settle!: (info: CloseInfo) => void;

  constructor(opened: Opened) {
    this.#id = opened.socket;
    this.#messages = opened.messages;
    this.url = opened.url;
    this.protocol = opened.protocol;
    this.closed = new Promise(resolve => {
      this.#settle = resolve;
    });
  }

  /** Sends a message: a string as a text message, bytes as a binary one (at most 196608 bytes). */
  async send(data: string | Uint8Array<ArrayBuffer>, options: Cancelable = {}): Promise<void> {
    const text = typeof data === 'string';
    const body = text ? encoder.encode(data) : data;
    if (body.length > MAX_SEND) throw new AlefError('INVALID_ARGUMENT', `A message to send is at most ${MAX_SEND} bytes.`);
    await call<null>('websocket.send', { socket: this.#id, text }, { signal: options.signal, body: body.length > 0 ? body : undefined });
  }

  async *[Symbol.asyncIterator](): AsyncGenerator<WebSocketMessage> {
    let kind: 'text' | 'binary' | null = null;
    let pieces: Uint8Array[] = [];
    let missing = 0;
    const finish = (): Uint8Array<ArrayBuffer> => {
      const bytes = new Uint8Array(pieces.reduce((total, piece) => total + piece.length, 0));
      let at = 0;
      for (const piece of pieces) {
        bytes.set(piece, at);
        at += piece.length;
      }
      pieces = [];
      return bytes;
    };
    try {
      for await (const frame of await openReadable(this.#messages)) {
        if (frame.kind === 'json') {
          const header = frame.value as { type: string; length?: number; code?: number; reason?: string; clean?: boolean };
          if (header.type === 'close') {
            this.#settle({ code: header.code ?? 1005, reason: header.reason ?? '', clean: header.clean ?? false });
            continue;
          }
          kind = header.type === 'text' ? 'text' : 'binary';
          missing = header.length ?? 0;
          pieces = [];
        } else if (kind !== null) {
          pieces.push(frame.data);
          missing -= frame.data.length;
        }
        if (kind !== null && missing <= 0) {
          const bytes = finish();
          const type = kind;
          kind = null;
          yield type === 'text' ? { type, data: decoder.decode(bytes) } : { type, data: bytes };
        }
      }
    } catch (error) {
      if (!this.#done) throw error;
    } finally {
      this.#settle({ code: 1006, reason: '', clean: false });
      await this.close().catch(() => undefined);
    }
  }

  /** Closes the connection: `code` is 1000 (the default) or 3000 to 4999, `reason` at most 123 bytes. Safe to call repeatedly. */
  async close(code?: number, reason?: string): Promise<void> {
    if (this.#done) return;
    this.#done = true;
    await call<null>('websocket.close', { socket: this.#id, code, reason });
  }
}

/** Connections to servers of WebSocket (RFC 6455), held against `permissions.net.http` (patterns like `wss://chat.example.com/*`). */
export const websocket = {
  connect: async (url: string, options: WebSocketOptions = {}): Promise<WebSocketConnection> => {
    const { protocols, headers, ca, timeout, signal } = options;
    const pairs = headers === undefined ? undefined : [...new Headers(headers)];
    return new WebSocketConnection(await call<Opened>('websocket.connect', { url, protocols, headers: pairs, ca, timeoutMs: timeout }, { signal }));
  },
};
