// SPDX-License-Identifier: MIT OR Apache-2.0
import { decodeBase64 } from '../core/base64.ts';
import { AlefError } from '../core/errors.ts';
import { openReadable } from '../core/stream.ts';
import { call } from '../core/transport.ts';
import { bytesOf, sinkOf } from '../core/web-stream.ts';
import type { Cancelable } from '../desktop/app.ts';

const encoder = new TextEncoder();

export interface Address {
  host: string;
  port: number;
}

export interface TlsOptions {
  /** The name the certificate of the server must have; the host when omitted. */
  serverName?: string;
  /** PEM certificates of the authorities to trust instead of the roots of Mozilla (they replace them). */
  ca?: string;
}

export interface ConnectOptions extends Cancelable {
  host: string;
  port: number;
  /** `true` for TLS with the roots of Mozilla, or its options. */
  tls?: boolean | TlsOptions;
  /** Milliseconds the connection (and the handshake of TLS) has; it ends with `TIMEOUT` after that. */
  timeout?: number;
}

export interface ListenOptions extends Cancelable {
  /** The address to listen on; this machine alone (`127.0.0.1`) when omitted. */
  host?: string;
  /** `0` or omitted: any free port, which `localAddress` tells. */
  port?: number;
}

export interface UdpOptions extends Cancelable {
  host?: string;
  port?: number;
}

export interface Datagram {
  data: Uint8Array<ArrayBuffer>;
  /** Who sent it. */
  host: string;
  port: number;
}

interface Opened {
  socket: number;
  read: number;
  write: number;
  localAddress: Address;
  remoteAddress: Address;
}

/** A TCP connection: `readable` and `writable` carry the bytes with backpressure; closing the writable ends the output only. */
export class TcpSocket {
  readonly readable: ReadableStream<Uint8Array>;
  readonly writable: WritableStream<Uint8Array>;
  readonly localAddress: Address;
  readonly remoteAddress: Address;
  #id: number;
  #closed = false;

  constructor(opened: Opened) {
    this.#id = opened.socket;
    this.localAddress = opened.localAddress;
    this.remoteAddress = opened.remoteAddress;
    this.readable = bytesOf(opened.read);
    this.writable = sinkOf(opened.write);
  }

  /** Closes the connection in both directions. Safe to call repeatedly. */
  async close(): Promise<void> {
    if (this.#closed) return;
    this.#closed = true;
    await call<null>('socket.close', { socket: this.#id });
  }
}

/** A TCP server: iterate it once for the connections it takes. */
export class TcpServer implements AsyncIterable<TcpSocket> {
  readonly localAddress: Address;
  #id: number;
  #accept: number;
  #closed = false;

  constructor(opened: { server: number; accept: number; localAddress: Address }) {
    this.#id = opened.server;
    this.#accept = opened.accept;
    this.localAddress = opened.localAddress;
  }

  async *[Symbol.asyncIterator](): AsyncGenerator<TcpSocket> {
    try {
      for await (const frame of await openReadable(this.#accept)) {
        if (frame.kind === 'json') yield new TcpSocket(frame.value as Opened);
      }
    } catch (error) {
      if (!this.#closed) throw error;
    }
  }

  /** Stops taking connections; the ones it gave stay open. Safe to call repeatedly. */
  async close(): Promise<void> {
    if (this.#closed) return;
    this.#closed = true;
    await call<null>('socket.close', { socket: this.#id });
  }
}

/** A UDP socket: iterate it once for the datagrams that arrive. */
export class UdpSocket implements AsyncIterable<Datagram> {
  readonly localAddress: Address;
  #id: number;
  #messages: number;
  #closed = false;

  constructor(opened: { socket: number; messages: number; localAddress: Address }) {
    this.#id = opened.socket;
    this.#messages = opened.messages;
    this.localAddress = opened.localAddress;
  }

  /** Sends one datagram (at most 65507 bytes) to `host:port`; `permissions.net.socket` must list `udp:host:port`. */
  async send(data: Uint8Array<ArrayBuffer> | string, host: string, port: number, options: Cancelable = {}): Promise<void> {
    const bytes = typeof data === 'string' ? encoder.encode(data) : data;
    await call<null>('socket.send', { socket: this.#id, host, port }, { signal: options.signal, body: bytes.length > 0 ? bytes : undefined });
  }

  async *[Symbol.asyncIterator](): AsyncGenerator<Datagram> {
    try {
      for await (const frame of await openReadable(this.#messages)) {
        if (frame.kind !== 'json') continue;
        const { host, port, data } = frame.value as { host: string; port: number; data: string };
        yield { host, port, data: decodeBase64(data) };
      }
    } catch (error) {
      if (!this.#closed) throw error;
    }
  }

  /** Releases the port. Safe to call repeatedly. */
  async close(): Promise<void> {
    if (this.#closed) return;
    this.#closed = true;
    await call<null>('socket.close', { socket: this.#id });
  }
}

/**
 * TCP and UDP. `permissions.net.socket` lists `tcp:host:port` to connect, `udp:host:port` to send a
 * datagram, `listen:host:port` to take a port of this machine (for a server, and for a UDP socket).
 */
export const socket = {
  connect: async (options: ConnectOptions): Promise<TcpSocket> => {
    const { host, port, tls, timeout, signal } = options;
    if (typeof host !== 'string' || !Number.isInteger(port)) throw new AlefError('INVALID_ARGUMENT', 'connect needs a host and a port.');
    return new TcpSocket(await call<Opened>('socket.connect', { host, port, tls, timeoutMs: timeout }, { signal }));
  },

  listen: async (options: ListenOptions = {}): Promise<TcpServer> => {
    const { host, port, signal } = options;
    return new TcpServer(await call<{ server: number; accept: number; localAddress: Address }>('socket.listen', { host, port }, { signal }));
  },

  udp: async (options: UdpOptions = {}): Promise<UdpSocket> => {
    const { host, port, signal } = options;
    return new UdpSocket(await call<{ socket: number; messages: number; localAddress: Address }>('socket.udp', { host, port }, { signal }));
  },
};
