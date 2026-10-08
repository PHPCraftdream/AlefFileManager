// SPDX-License-Identifier: MIT OR Apache-2.0
import type { Transport } from '../types.ts';
export type Streams = { input: ReadableStream<Uint8Array>; output: WritableStream<Uint8Array> } |
  { readable: ReadableStream<Uint8Array>; writable: WritableStream<Uint8Array> };
/** Newline-delimited JSON, never Content-Length framing. EOF must end at a newline. */
export function stdio(streams: Streams, maxBytes = 1024 * 1024): Transport {
  const reader = ('input' in streams ? streams.input : streams.readable).getReader();
  const writer = ('output' in streams ? streams.output : streams.writable).getWriter();
  const encoder = new TextEncoder();
  let closed = false;
  let started = false;
  return {
    async send(message) {
      if (closed) throw new Error('stdio is closed.');
      const bytes = encoder.encode(`${JSON.stringify(message)}\n`);
      if (bytes.length > maxBytes) throw new Error('stdio message is too large.');
      await writer.write(bytes);
    },
    start(receive, end) {
      if (started) throw new Error('stdio was started already.');
      started = true;
      void (async () => {
        let pending = new Uint8Array(0);
        try {
          for (;;) {
            const { done, value } = await reader.read();
            if (done) {
              if (pending.length) throw new Error('stdio ended with an incomplete message.');
              end(); break;
            }
            const joined = new Uint8Array(pending.length + value.length);
            joined.set(pending); joined.set(value, pending.length);
            let start = 0;
            for (let index = 0; index < joined.length; index++) {
              if (joined[index] !== 10) continue;
              if (index - start > maxBytes) throw new Error('stdio message is too large.');
              const text = new TextDecoder('utf-8', { fatal: true }).decode(joined.subarray(start, index)).replace(/\r$/, '');
              start = index + 1;
              if (!text.trim()) continue;
              try { receive(JSON.parse(text)); } catch (error) {
                if (!(error instanceof SyntaxError)) throw error;
                await writer.write(encoder.encode(`${JSON.stringify({ jsonrpc: '2.0', id: null, error: { code: -32700, message: 'Parse error.' } })}\n`));
              }
            }
            pending = joined.slice(start);
            if (pending.length > maxBytes) throw new Error('stdio message is too large.');
          }
        } catch (error) { if (!closed) end(error); }
        finally { reader.releaseLock(); }
      })();
    },
    async close() {
      if (closed) return;
      closed = true;
      // Abort rather than wait for a peer that no longer consumes stdout.
      await Promise.allSettled([reader.cancel(), writer.abort()]);
      writer.releaseLock();
      if (!started) reader.releaseLock();
    },
  };
}
