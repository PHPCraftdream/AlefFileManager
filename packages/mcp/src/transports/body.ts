// SPDX-License-Identifier: MIT OR Apache-2.0
import { duration } from '../types.ts';
export async function bounded<T>(promise: Promise<T>, timeout: number, signal?: AbortSignal, cancel?: () => void): Promise<T> {
  duration(timeout);
  return new Promise((resolve, reject) => {
    const stop = (error: unknown) => { cancel?.(); finish(); reject(error); };
    const abort = () => stop(signal?.reason ?? new Error('Operation cancelled.'));
    const timer = setTimeout(() => stop(new Error('MCP operation timed out.')), timeout);
    const finish = () => { clearTimeout(timer); signal?.removeEventListener('abort', abort); };
    promise.then(value => { finish(); resolve(value); }, error => { finish(); reject(error); });
    if (signal?.aborted) { abort(); return; }
    signal?.addEventListener('abort', abort, { once: true });
  });
}
export async function readBody(body: ReadableStream<Uint8Array> | null, limit: number, timeout: number, signal?: AbortSignal): Promise<string> {
  if (!body) return '';
  const reader = body.getReader();
  const decoder = new TextDecoder('utf-8', { fatal: true });
  let length = 0;
  let text = '';
  try {
    return await bounded((async () => {
      for (;;) {
        const { done, value } = await reader.read();
        if (done) return text + decoder.decode();
        length += value.length;
        if (length > limit) throw new Error('Body too large.');
        text += decoder.decode(value, { stream: true });
      }
    })(), timeout, signal, () => { void reader.cancel().catch(() => {}); });
  } finally {
    // Cancel on malformed/oversized bodies as well, not just on timeout.
    void reader.cancel().catch(() => {});
    reader.releaseLock();
  }
}
