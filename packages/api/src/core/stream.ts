// SPDX-License-Identifier: MIT OR Apache-2.0
// Frame stream: [kind u8][len u32 LE][payload]; 1 json, 2 binary, 3 end, 4 error. Every payload
// byte of a json or binary frame is credit: the reader acks consumed bytes, the runtime pauses a
// producer whose window (1 MiB) is used up.
import { AlefError, errorFromBody, errorFromResponse } from './errors.ts';
import { session } from './handshake.ts';
import { bearer, call, send } from './transport.ts';

const HEADER = 5;
const MAX_FRAME = 16 * 1024 * 1024;
const ACK_BATCH = 256 * 1024;
const KIND = { json: 1, binary: 2, end: 3, error: 4 } as const;

export type StreamFrame =
  | { kind: 'json'; value: unknown }
  | { kind: 'binary'; data: Uint8Array };

export interface Readable extends AsyncIterable<StreamFrame> {
  readonly id: number;
  /** Cancels the stream; the runtime closes the source. Safe to call repeatedly. */
  close(): Promise<void>;
}

export interface Writable {
  readonly id: number;
  /** Resolves once the runtime accepted the bytes (backpressure); large chunks are split. */
  write(chunk: Uint8Array<ArrayBuffer>): Promise<void>;
  /** Ends the stream normally. */
  end(): Promise<void>;
  /** Ends the stream with a closed error on the runtime side. */
  abort(): Promise<void>;
}

interface RawFrame {
  kind: number;
  payload: Uint8Array;
}

function concat(first: Uint8Array, second: Uint8Array): Uint8Array {
  if (first.length === 0) return second;
  const merged = new Uint8Array(first.length + second.length);
  merged.set(first);
  merged.set(second, first.length);
  return merged;
}

class FrameDecoder {
  #reader: ReadableStreamDefaultReader<Uint8Array>;
  #buffer: Uint8Array = new Uint8Array(0);

  constructor(reader: ReadableStreamDefaultReader<Uint8Array>) {
    this.#reader = reader;
  }

  /** Next frame, or `null` when the body ended. Chunk boundaries of the body do not matter. */
  async next(): Promise<RawFrame | null> {
    for (;;) {
      if (this.#buffer.length >= HEADER) {
        const view = new DataView(this.#buffer.buffer, this.#buffer.byteOffset, this.#buffer.byteLength);
        const length = view.getUint32(1, true);
        if (length > MAX_FRAME) throw new AlefError('INTERNAL', 'Stream frame is too large.');
        if (this.#buffer.length >= HEADER + length) {
          const frame = { kind: this.#buffer[0], payload: this.#buffer.slice(HEADER, HEADER + length) };
          this.#buffer = this.#buffer.subarray(HEADER + length);
          return frame;
        }
      }
      const { done, value } = await this.#reader.read();
      if (done) return null;
      this.#buffer = concat(this.#buffer, value);
    }
  }
}

class StreamReadable implements Readable {
  readonly id: number;
  #frames: FrameDecoder;
  #controller: AbortController;
  #unacked = 0;
  #closed = false;

  constructor(id: number, frames: FrameDecoder, controller: AbortController) {
    this.id = id;
    this.#frames = frames;
    this.#controller = controller;
  }

  async #ack(force: boolean): Promise<void> {
    if (this.#unacked === 0 || (!force && this.#unacked < ACK_BATCH)) return;
    const bytes = this.#unacked;
    this.#unacked = 0;
    try {
      await call('runtime.stream.ack', { id: this.id, bytes });
    } catch {
      // the stream is already gone; the read below reports the reason
    }
  }

  async *[Symbol.asyncIterator](): AsyncGenerator<StreamFrame> {
    try {
      for (;;) {
        const frame = await this.#frames.next();
        if (frame === null || frame.kind === KIND.end) {
          this.#closed = true; // ended by the runtime: nothing left to cancel
          return;
        }
        if (frame.kind === KIND.error) {
          throw errorFromBody(JSON.parse(new TextDecoder().decode(frame.payload)));
        }
        this.#unacked += frame.payload.length;
        await this.#ack(false);
        if (frame.kind === KIND.json) {
          yield { kind: 'json', value: JSON.parse(new TextDecoder().decode(frame.payload)) };
        } else if (frame.kind === KIND.binary) {
          yield { kind: 'binary', data: frame.payload };
        } else {
          throw new AlefError('INTERNAL', `Unknown stream frame kind ${frame.kind}.`);
        }
      }
    } finally {
      await this.close();
    }
  }

  async close(): Promise<void> {
    if (this.#closed) return;
    this.#closed = true;
    this.#controller.abort();
    try {
      await call('runtime.stream.close', { id: this.id });
    } catch {
      // already ended or closed by the runtime
    }
  }
}

/** Opens the outgoing stream `id` (the `{ stream }` reply of a command) for reading. */
export async function openReadable(id: number, options: { signal?: AbortSignal } = {}): Promise<Readable> {
  const { token } = await session();
  options.signal?.throwIfAborted();
  const controller = new AbortController();
  const response = await send(`native://stream/${id}`, { headers: bearer(token), signal: controller.signal });
  if (!response.ok) throw await errorFromResponse(response);
  if (!response.body) throw new AlefError('TRANSPORT', 'The stream has no body.');
  const readable = new StreamReadable(id, new FrameDecoder(response.body.getReader()), controller);
  options.signal?.addEventListener('abort', () => void readable.close(), { once: true });
  return readable;
}

/** Opens the incoming stream `id` (created by a command) for writing bytes to the runtime. */
export async function openWritable(id: number, options: { signal?: AbortSignal } = {}): Promise<Writable> {
  const { info } = await session();
  options.signal?.throwIfAborted();
  const chunkSize = info.limits.chunkSize;
  return {
    id,
    async write(chunk: Uint8Array<ArrayBuffer>): Promise<void> {
      for (let at = 0; at < chunk.length; at += chunkSize) {
        await call('runtime.stream.write', { id }, { body: chunk.subarray(at, at + chunkSize), signal: options.signal });
      }
    },
    async end(): Promise<void> {
      await call('runtime.stream.end', { id }, { signal: options.signal });
    },
    async abort(): Promise<void> {
      await call('runtime.stream.close', { id }, { signal: options.signal });
    },
  };
}
