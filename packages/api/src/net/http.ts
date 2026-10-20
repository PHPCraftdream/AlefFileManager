// SPDX-License-Identifier: MIT OR Apache-2.0
import { AlefError } from '../core/errors.ts';
import { openReadable } from '../core/stream.ts';
import { call } from '../core/transport.ts';
import { bytesOf } from '../core/web-stream.ts';
import type { Cancelable } from '../core/events.ts';
import { type Body, collect, pairs, UNARY_BODY, upload } from './body.ts';
import { HttpServer, type ServeOptions } from './http-server.ts';

export { HttpServer, ServerRequest, type ServeOptions, type ServerResponse, type UpgradeOptions } from './http-server.ts';

export interface HttpRequestOptions extends Cancelable {
  /** `GET` when omitted. */
  method?: string;
  headers?: HeadersInit;
  body?: Body;
  /** Milliseconds the server has to begin its answer; the request ends with `TIMEOUT` after that. */
  timeout?: number;
  /** `follow` (up to 10 redirects, each held against the scope again) or `manual`: the redirect is the answer. */
  redirect?: 'follow' | 'manual';
  jar?: CookieJar;
  proxy?: string;
}

export interface DownloadOptions extends Cancelable {
  headers?: HeadersInit;
  timeout?: number;
  jar?: CookieJar;
  proxy?: string;
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

/** Cookie storage shared by HTTP requests. */
export class CookieJar {
  #id: number;
  constructor(id: number) { this.#id = id; }
  get id(): number { return this.#id; }
  clear(): Promise<void> { return call('http.cookieJar.clear', { jar: this.#id }); }
  list(url: string): Promise<Array<{ name: string; value: string }>> { return call('http.cookieJar.list', { jar: this.#id, url }); }
  close(): Promise<void> { return call('http.cookieJar.close', { jar: this.#id }); }
}

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
    return collect(this.body);
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

/**
 * Requests to servers on the network, which no CORS restricts: the manifest lists the addresses
 * (`permissions.net.http`, patterns like `https://api.example.com/*`) and the user allows them.
 * `serve` takes a port of this machine instead (`permissions.net.socket`: `listen:127.0.0.1:*`).
 */
export const http = {
  request: async (url: string, options: HttpRequestOptions = {}): Promise<HttpResponse> => {
    const { method, headers, body, timeout, redirect, signal, jar, proxy } = options;
    const args = { url, method, headers: pairs(headers), timeoutMs: timeout, redirect, ...(jar ? { jar: jar.id } : {}), ...(proxy !== undefined ? { proxy } : {}) };
    const small = typeof body === 'string' ? encoder.encode(body) : body instanceof Uint8Array ? body : undefined;
    if (body === undefined || (small !== undefined && small.length <= UNARY_BODY)) {
      const head = await call<Head>('http.request', args, { signal, body: small && small.length > 0 ? small : undefined });
      return new HttpResponse(head);
    }
    const { request, upload: stream } = await call<{ request: number; upload: number }>('http.start', args, { signal });
    await upload(stream, body, { signal });
    return new HttpResponse(await call<Head>('http.response', { request }, { signal }));
  },

  /** Opens an empty cookie jar. */
  cookieJar: async (): Promise<CookieJar> => new CookieJar((await call<{ jar: number }>('http.cookieJar', {})).jar),

  /** Takes a port of this machine and gives the requests that come to it; see `HttpServer`. */
  serve: async (options: ServeOptions = {}): Promise<HttpServer> => {
    const { answerTimeout, signal, ...rest } = options;
    const args = { ...rest, answerTimeoutMs: answerTimeout };
    return new HttpServer(await call<ConstructorParameters<typeof HttpServer>[0]>('http.serve', args, { signal }));
  },

  /** Fills the file at `path` (it needs `permissions.fs.write`); a download that fails or is aborted leaves no file. */
  download: async (url: string, path: string, options: DownloadOptions = {}): Promise<void> => {
    const { headers, timeout, onProgress, signal, jar, proxy } = options;
    const args = { url, path, headers: pairs(headers), timeoutMs: timeout, ...(jar ? { jar: jar.id } : {}), ...(proxy !== undefined ? { proxy } : {}) };
    const { stream } = await call<{ stream: number }>('http.download', args, { signal });
    for await (const frame of await openReadable(stream, { signal })) {
      if (frame.kind === 'json') {
        const { received, total } = frame.value as { received: number; total: number | null };
        onProgress?.({ received, total });
      }
    }
  },
};
