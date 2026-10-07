// SPDX-License-Identifier: MIT OR Apache-2.0
// The streams of the runtime as the streams of the web platform, with backpressure both ways.
import { openReadable, openWritable, type Readable, type Writable } from './stream.ts';

/** The bytes of the runtime stream `id`; a stream that is cancelled is closed on the runtime side. */
export function bytesOf(id: number): ReadableStream<Uint8Array> {
  let readable: Readable | undefined;
  let frames: AsyncIterator<{ kind: string; data?: Uint8Array }> | undefined;
  return new ReadableStream<Uint8Array>({
    async start() {
      readable = await openReadable(id);
      frames = readable[Symbol.asyncIterator]();
    },
    async pull(controller) {
      for (;;) {
        const next = await frames!.next();
        if (next.done) {
          controller.close();
          return;
        }
        if (next.value.kind === 'binary' && next.value.data) {
          controller.enqueue(next.value.data);
          return;
        }
      }
    },
    async cancel() {
      await readable?.close();
    },
  });
}

/** A sink that writes to the incoming runtime stream `id`: closing it ends the stream, aborting it closes the stream. */
export function sinkOf(id: number): WritableStream<Uint8Array> {
  let writable: Writable | undefined;
  return new WritableStream<Uint8Array>({
    async start() {
      writable = await openWritable(id);
    },
    async write(chunk) {
      await writable!.write(chunk as Uint8Array<ArrayBuffer>);
    },
    async close() {
      await writable!.end();
    },
    async abort() {
      await writable!.abort();
    },
  });
}
