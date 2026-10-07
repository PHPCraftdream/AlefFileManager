// SPDX-License-Identifier: MIT OR Apache-2.0
import { AlefError } from '../core/errors.ts';
import { openReadable, openWritable } from '../core/stream.ts';
import { call } from '../core/transport.ts';
import { bytesOf } from '../core/web-stream.ts';
import type { Cancelable } from '../desktop/app.ts';

/** A body up to this size travels with the call; a bigger one, and a stream, go up as a stream. */
const UNARY_BODY = 192 * 1024;

type Body = string | Uint8Array<ArrayBuffer> | ReadableStream<Uint8Array<ArrayBuffer>>;

export interface HttpRequestOptions extends Cancelable {
  /** `GET` when omitted. */
  method?: string;
  headers?: HeadersInit;
  body?: Body;
  /** Milliseconds the server has to begin its answer; the request ends with `TIMEOUT` after that. */
  timeout?: number;
  /** `follow` (up to 10 redirects, each held against the scope again) or `manual`: the redirect is the answer. */
  redirect?: 'follow' | 'manual';
}

export interface DownloadOptions extends Cancelable {
  headers?: HeadersInit;
  timeout?: number;
  /** Called as the bytes come: how many so far, and how many in all when the server says. */
  onProgress?: (progress: { received: number; total: number | null }) => void;
}

interface Head {
  status: number;
  statusText: string;
  url: string;
  redirected: boolean;
  headers: Array<[string, string]>;
  stream: number | null;
}

const encoder = new TextEncoder();
const decoder = new TextDecoder();

/** The answer of a server. The body is read once, as a stream or through `bytes`, `text` or `json`. */
export class HttpResponse {
  readonly status: number;
  readonly statusText: string;
  /** The address the answer came from, after the redirects. */
  readonly url: string;
  readonly redirected: boolean;
  readonly headers: Headers;
  #stream: number | null;
  #used = false;

  constructor(head: Head) {
    this.status = head.status;
    this.statusText = head.statusText;
    this.url = head.url;
    this.redirected = head.redirected;
    this.headers = new Headers();
    for (const [name, value] of head.headers) this.headers.append(name, value);
    this.#stream = head.stream;
  }

  get ok(): boolean {
    return this.status >= 200 && this.status < 300;
  }

  get bodyUsed(): boolean {
    return this.#used;
  }

  /** The body as a stream with backpressure: a big one passes with a bounded buffer. `null` when the answer has none. */
  get body(): ReadableStream<Uint8Array> | null {
    if (this.#stream === null || this.#used) return null;
    this.#used = true;
    return bytesOf(this.#stream);
  }

  /** The whole body. */
  async bytes(): Promise<Uint8Array<ArrayBuffer>> {
    if (this.#used) throw new AlefError('INVALID_ARGUMENT', 'The body was read already.');
    const body = this.body;
    if (body === null) return new Uint8Array(0);
    const pieces: Uint8Array[] = [];
    let length = 0;
    const reader = body.getReader();
    for (;;) {
      const { done, value } = await reader.read();
      if (done) break;
      pieces.push(value);
      length += value.length;
    }
    const all = new Uint8Array(length);
    let at = 0;
    for (const piece of pieces) {
      all.set(piece, at);
      at += piece.length;
    }
    return all;
  }

  /** The body as UTF-8 text. */
  async text(): Promise<string> {
    return decoder.decode(await this.bytes());
  }

  /** The body as JSON. */
  async json(): Promise<unknown> {
    return JSON.parse(await this.text());
  }
}

const pairs = (headers: HeadersInit | undefined): Array<[string, string]> => (headers === undefined ? [] : [...new Headers(headers)]);

/** Writes a stream or a big body up to the runtime, which sends it as it comes. */
async function upload(id: number, body: Body, options: Cancelable): Promise<void> {
  const writable = await openWritable(id);
  try {
    if (typeof body === 'string') await writable.write(encoder.encode(body));
    else if (body instanceof Uint8Array) await writable.write(body);
    else {
      const reader = body.getReader();
      for (;;) {
        if (options.signal?.aborted) throw options.signal.reason;
        const { done, value } = await reader.read();
        if (done) break;
        await writable.write(value);
      }
    }
    await writable.end();
  } catch (error) {
    await writable.abort().catch(() => undefined);
    throw error;
  }
}

/**
 * Requests to servers on the network, which no CORS restricts: the manifest lists the addresses
 * (`permissions.net.http`, patterns like `https://api.example.com/*`) and the user allows them.
 */
export const http = {
  request: async (url: string, options: HttpRequestOptions = {}): Promise<HttpResponse> => {
    const { method, headers, body, timeout, redirect, signal } = options;
    const args = { url, method, headers: pairs(headers), timeoutMs: timeout, redirect };
    const small = typeof body === 'string' ? encoder.encode(body) : body instanceof Uint8Array ? body : undefined;
    if (body === undefined || (small !== undefined && small.length <= UNARY_BODY)) {
      const head = await call<Head>('http.request', args, { signal, body: small && small.length > 0 ? small : undefined });
      return new HttpResponse(head);
    }
    const { request, upload: stream } = await call<{ request: number; upload: number }>('http.start', args, { signal });
    await upload(stream, body, { signal });
    return new HttpResponse(await call<Head>('http.response', { request }, { signal }));
  },

  /** Fills the file at `path` (it needs `permissions.fs.write`); a download that fails or is aborted leaves no file. */
  download: async (url: string, path: string, options: DownloadOptions = {}): Promise<void> => {
    const { headers, timeout, onProgress, signal } = options;
    const { stream } = await call<{ stream: number }>('http.download', { url, path, headers: pairs(headers), timeoutMs: timeout }, { signal });
    for await (const frame of await openReadable(stream, { signal })) {
      if (frame.kind === 'json') {
        const { received, total } = frame.value as { received: number; total: number | null };
        onProgress?.({ received, total });
      }
    }
  },
};
