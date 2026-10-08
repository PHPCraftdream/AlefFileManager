// SPDX-License-Identifier: MIT OR Apache-2.0
import { openWritable } from '../core/stream.ts';
import type { Cancelable } from '../desktop/app.ts';

/** A body up to this size travels with the call; a bigger one, and a stream, go up as a stream. */
export const UNARY_BODY = 192 * 1024;

export type Body = string | Uint8Array<ArrayBuffer> | ReadableStream<Uint8Array<ArrayBuffer>>;

const encoder = new TextEncoder();

export const pairs = (headers: HeadersInit | undefined): Array<[string, string]> => (headers === undefined ? [] : [...new Headers(headers)]);

/** Writes a stream or a big body up to the runtime, which sends it as it comes. */
export async function upload(id: number, body: Body, options: Cancelable): Promise<void> {
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

/** The bytes of a stream, whole. */
export async function collect(body: ReadableStream<Uint8Array> | null): Promise<Uint8Array<ArrayBuffer>> {
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
