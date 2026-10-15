// SPDX-License-Identifier: MIT OR Apache-2.0
import { AlefError, errorFromResponse } from './errors.ts';

/** Transport protocol version this client speaks. */
const PROTOCOL = 1;

export interface RuntimeLimits {
  maxUnaryBody: number;
  maxBulkBody: number;
  streamWindow: number;
  chunkSize: number;
  maxResources: number;
  maxConcurrentCalls: number;
}

export interface RuntimeInfo {
  protocol: number;
  runtime: string;
  platform: string;
  arch: string;
  /** Modules (the first segment of every command name) this runtime serves. */
  modules: string[];
  limits: RuntimeLimits;
}

export interface Session {
  /** Bearer token of this document's session; never handed to application code. */
  token: string;
  info: RuntimeInfo;
}

let pending: Promise<Session> | undefined;

// The runtime hands the bootstrap capability to the page in the URL fragment.
const bootstrapToken = (): string =>
  new URLSearchParams((globalThis.location?.hash ?? '').slice(1)).get('capability') ?? '';

// Not tied to any caller's signal: every call of the document shares this one handshake.
async function hello(): Promise<Session> {
  const bootstrap = bootstrapToken();
  if (!bootstrap) {
    throw new AlefError('NOT_AVAILABLE', 'No Alef runtime: start the application with npm run dev or npm start.');
  }
  let response: Response;
  try {
    response = await fetch('native://call/runtime.hello', {
      method: 'POST',
      headers: { Authorization: `Bearer ${bootstrap}`, 'Content-Type': 'application/json' },
      body: '{}',
    });
  } catch (error) {
    throw new AlefError('TRANSPORT', error instanceof Error ? error.message : 'The Alef runtime is unreachable.');
  }
  if (!response.ok) throw await errorFromResponse(response);
  const { token, ...info } = await response.json() as RuntimeInfo & { token: string };
  if (info.protocol !== PROTOCOL) {
    throw new AlefError('NOT_AVAILABLE', `Runtime protocol ${String(info.protocol)} is not supported (client speaks ${PROTOCOL}).`);
  }
  return { token, info };
}

/** The session of this document; the handshake runs once and a failure is not remembered. */
export function session(): Promise<Session> {
  pending ??= hello().catch((error: unknown) => {
    pending = undefined;
    throw error;
  });
  return pending;
}

/** Performs the handshake (once per document) and describes the runtime. */
export async function connect(): Promise<RuntimeInfo> {
  return (await session()).info;
}

export interface CallOptions {
  /** Binary request body; `args` then travel in the `x-alef-args` header. */
  body?: Uint8Array<ArrayBuffer>;
  signal?: AbortSignal;
}

/** `fetch` that reports a failure of the transport itself as `AlefError('TRANSPORT')`. */
export async function send(url: string, init: RequestInit): Promise<Response> {
  try {
    return await fetch(url, init);
  } catch (error) {
    if (init.signal?.aborted) throw error;
    throw new AlefError('TRANSPORT', error instanceof Error ? error.message : 'The Alef runtime is unreachable.');
  }
}

export const bearer = (token: string): Record<string, string> => ({ Authorization: `Bearer ${token}` });

/**
 * Calls a native command. Resolves with the parsed JSON reply, or with a `Uint8Array` when the
 * command replies with bytes; rejects with `AlefError` (`PERMISSION_DENIED`, `NOT_FOUND`, ...).
 * Aborting `signal` aborts the request and cancels the command in the runtime.
 */
export async function call<T = unknown>(command: string, args: unknown = null, options: CallOptions = {}): Promise<T> {
  const { token } = await session();
  const headers = bearer(token);
  const init: RequestInit = { method: 'POST', headers, signal: options.signal };
  if (options.body) {
    headers['Content-Type'] = 'application/octet-stream';
    headers['x-alef-args'] = encodeURIComponent(JSON.stringify(args));
    init.body = options.body;
  } else {
    headers['Content-Type'] = 'application/json';
    init.body = JSON.stringify(args);
  }
  const response = await send(`native://call/${encodeURIComponent(command)}`, init);
  if (!response.ok) throw await errorFromResponse(response);
  if ((response.headers.get('content-type') ?? '').startsWith('application/octet-stream')) {
    return new Uint8Array(await response.arrayBuffer()) as T;
  }
  return await response.json() as T;
}
