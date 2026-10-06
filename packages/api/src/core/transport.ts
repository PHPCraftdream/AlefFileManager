// SPDX-License-Identifier: MIT OR Apache-2.0
import { AlefError, errorFromResponse } from './errors.ts';
import { session } from './handshake.ts';

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
