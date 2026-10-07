// SPDX-License-Identifier: MIT OR Apache-2.0
import type { DirEntry, FileStat, WatchEvent } from '../../types/index.ts';
import { openReadable, openWritable, type Readable, type Writable } from '../core/stream.ts';
import { call } from '../core/transport.ts';
import type { Cancelable } from '../desktop/app.ts';

const encoder = new TextEncoder();

export interface FileOpenOptions extends Cancelable {
  /** Read from the file; the default when nothing else is asked for. */
  read?: boolean;
  write?: boolean;
  /** Every write goes to the end of the file. */
  append?: boolean;
  /** Make the file when it is not there (needs `write` or `append`). */
  create?: boolean;
  /** Empty the file (needs `write`). */
  truncate?: boolean;
  /** Make the file and fail with `ALREADY_EXISTS` when it is there. */
  createNew?: boolean;
}

export interface StreamOptions {
  /** Where in the file the stream starts; the position of the handle when absent. */
  position?: number;
}

export interface ReadStreamOptions extends StreamOptions {
  /** How many bytes the stream gives; to the end of the file when absent. */
  length?: number;
}

export interface WatchOptions extends Cancelable {
  /** Watch the folders below as well. */
  recursive?: boolean;
}

/**
 * An open file. It belongs to the document that opened it and is closed with it. Read and write
 * piece by piece, or through `readable` and `writable`, which carry backpressure, so that a file of
 * any size passes with a bounded buffer: `await source.readable.pipeTo(target.writable)`.
 */
export class FileHandle {
  readonly id: number;
  #readable: ReadableStream<Uint8Array> | undefined;
  #writable: WritableStream<Uint8Array<ArrayBuffer>> | undefined;

  constructor(id: number) {
    this.id = id;
  }

  /** The rest of the file as a stream, from the position of the handle. */
  get readable(): ReadableStream<Uint8Array> {
    this.#readable ??= this.readStream();
    return this.#readable;
  }

  /** A stream that writes at the position of the handle; closing it waits until all of it is in the file. */
  get writable(): WritableStream<Uint8Array<ArrayBuffer>> {
    this.#writable ??= this.writeStream();
    return this.#writable;
  }

  /** A stream of the file from `position`, `length` bytes of it. */
  readStream(options: ReadStreamOptions = {}): ReadableStream<Uint8Array> {
    const handle = this.id;
    let readable: Readable | undefined;
    let frames: AsyncIterator<{ kind: string; data?: Uint8Array }> | undefined;
    return new ReadableStream<Uint8Array>({
      async start() {
        const { stream } = await call<{ stream: number }>('fs.readStream', { handle, ...options });
        readable = await openReadable(stream);
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

  /** A stream that writes at `position` (and goes on from there), or at the position of the handle. */
  writeStream(options: StreamOptions = {}): WritableStream<Uint8Array<ArrayBuffer>> {
    const handle = this.id;
    let writable: Writable | undefined;
    return new WritableStream<Uint8Array<ArrayBuffer>>({
      async start() {
        const { stream } = await call<{ stream: number }>('fs.writeStream', { handle, ...options });
        writable = await openWritable(stream);
      },
      async write(chunk) {
        await writable!.write(chunk);
      },
      async close() {
        await writable!.end();
        // The runtime has written what it was sent when this returns; a failure on the way is its answer.
        await call<void>('fs.settle', { handle });
      },
      async abort() {
        await writable?.abort();
        await call<void>('fs.settle', { handle }).catch(() => undefined);
      },
    });
  }

  /** Up to `length` bytes (16 MiB at most) from `position`, or from where the handle is, which then moves on; empty at the end. */
  read(length: number, position?: number, options: Cancelable = {}): Promise<Uint8Array> {
    return call<Uint8Array>('fs.read', { handle: this.id, length, position }, options);
  }

  /** Writes all of `data` at `position`, or where the handle is, which then moves on; says how much it wrote. */
  async write(data: Uint8Array<ArrayBuffer>, position?: number, options: Cancelable = {}): Promise<number> {
    const { written } = await call<{ written: number }>('fs.write', { handle: this.id, position }, { ...options, body: data });
    return written;
  }

  stat(options: Cancelable = {}): Promise<FileStat> {
    return call<FileStat>('fs.fstat', { handle: this.id }, options);
  }

  truncate(length: number, options: Cancelable = {}): Promise<void> {
    return call<void>('fs.truncate', { handle: this.id, length }, options);
  }

  /** Waits until the system has the data on the disk. */
  sync(options: Cancelable = {}): Promise<void> {
    return call<void>('fs.sync', { handle: this.id }, options);
  }

  /** Closes the file; close `writable` first when it was used, so that nothing is cut off. */
  close(options: Cancelable = {}): Promise<void> {
    return call<void>('fs.close', { handle: this.id }, options);
  }
}

// Where the language has `await using` the handle closes itself; older typings do not know the symbol.
const asyncDispose = (Symbol as unknown as { asyncDispose?: symbol }).asyncDispose;
if (typeof asyncDispose === 'symbol') {
  Object.defineProperty(FileHandle.prototype, asyncDispose, {
    value(this: FileHandle): Promise<void> {
      return this.close();
    },
  });
}

export interface ReadTextOptions extends Cancelable {
  /** A label `TextDecoder` knows: `utf-8` (default), `utf-16le`, `latin1`, ... */
  encoding?: string;
}

export interface WriteOptions extends Cancelable {
  /** Add to the end instead of replacing what is there. */
  append?: boolean;
  /** Make the file when it is not there (default); `false` fails with `NOT_FOUND` instead. */
  create?: boolean;
}

export interface TreeOptions extends Cancelable {
  /** `mkdir`: make the parents too and accept that the folder is there. `remove`: with what is inside. */
  recursive?: boolean;
}

/**
 * Files and folders. A path must lie inside `permissions.fs.read` (reading, listing, looking at) or
 * `permissions.fs.write` (changing), or be one the user picked in a dialog; anything else fails with
 * `PERMISSION_DENIED`, a link that leads out of the scope included. A path is looked at as it is
 * spelled by the platform. Whole files travel in one call, up to 64 MiB.
 *
 * Failures carry the code of their cause: `NOT_FOUND`, `ALREADY_EXISTS`, `NOT_A_DIRECTORY`,
 * `IS_A_DIRECTORY`, `DIRECTORY_NOT_EMPTY`, `BUSY`.
 */
export const fs = {
  /** The whole file. */
  readBytes: (path: string, options: Cancelable = {}): Promise<Uint8Array> =>
    call<Uint8Array>('fs.readFile', { path }, options),

  /** The whole file as text. A byte order mark at the start is kept. */
  readText: async (path: string, options: ReadTextOptions = {}): Promise<string> => {
    const { encoding = 'utf-8', ...rest } = options;
    const bytes = await call<Uint8Array>('fs.readFile', { path }, rest);
    return new TextDecoder(encoding, { ignoreBOM: true }).decode(bytes);
  },

  writeBytes: (path: string, data: Uint8Array<ArrayBuffer>, options: WriteOptions = {}): Promise<void> => {
    const { append, create, ...rest } = options;
    return call<void>('fs.writeFile', { path, append, create }, { ...rest, body: data });
  },

  /** Writes `text` as UTF-8. */
  writeText: (path: string, text: string, options: WriteOptions = {}): Promise<void> =>
    fs.writeBytes(path, encoder.encode(text), options),

  /** What is at the path, a link looked through. */
  stat: (path: string, options: Cancelable = {}): Promise<FileStat> =>
    call<FileStat>('fs.stat', { path }, options),

  /** What is at the path, a link looked at: its `kind` is `symlink`. */
  lstat: (path: string, options: Cancelable = {}): Promise<FileStat> =>
    call<FileStat>('fs.lstat', { path }, options),

  /** The entries of a folder, sorted by name; up to 100000 of them. */
  readDir: (path: string, options: Cancelable = {}): Promise<DirEntry[]> =>
    call<DirEntry[]>('fs.readDir', { path }, options),

  exists: (path: string, options: Cancelable = {}): Promise<boolean> =>
    call<boolean>('fs.exists', { path }, options),

  mkdir: (path: string, options: TreeOptions = {}): Promise<void> => {
    const { recursive, ...rest } = options;
    return call<void>('fs.mkdir', { path, recursive }, rest);
  },

  /** Removes a file, a link (not what it leads to) or a folder; a folder that is not empty needs `recursive`. */
  remove: (path: string, options: TreeOptions = {}): Promise<void> => {
    const { recursive, ...rest } = options;
    return call<void>('fs.remove', { path, recursive }, rest);
  },

  /** Moves or renames; both ends must lie in `fs.write`. */
  rename: (from: string, to: string, options: Cancelable = {}): Promise<void> =>
    call<void>('fs.rename', { from, to }, options),

  /** Copies a file, or a folder with everything in it (the target must not be there, links inside are refused). */
  copy: (from: string, to: string, options: Cancelable = {}): Promise<void> =>
    call<void>('fs.copy', { from, to }, options),

  /** Opens a file; the handle must be closed (it is, with the document). */
  open: async (path: string, options: FileOpenOptions = {}): Promise<FileHandle> => {
    const { signal, ...flags } = options;
    const { handle } = await call<{ handle: number }>('fs.open', { path, ...flags }, { signal });
    return new FileHandle(handle);
  },

  /** The entries of a big folder, a few hundred at a time, in the order of the disk. */
  readDirStream: async function* (path: string, options: Cancelable = {}): AsyncGenerator<DirEntry> {
    const { stream } = await call<{ stream: number }>('fs.readDirStream', { path }, options);
    for await (const frame of await openReadable(stream, options)) {
      if (frame.kind === 'json') yield* frame.value as DirEntry[];
    }
  },

  /**
   * What happens to a file or a folder, as events: `create`, `modify`, `remove`, `rename` (and
   * `overflow` when events were lost: look again). Events of a short moment are put together. Leaving
   * the loop ends the watch.
   */
  watch: async function* (path: string, options: WatchOptions = {}): AsyncGenerator<WatchEvent> {
    const { recursive, ...rest } = options;
    const { stream } = await call<{ stream: number }>('fs.watch', { path, recursive }, rest);
    for await (const frame of await openReadable(stream, rest)) {
      if (frame.kind === 'json') yield frame.value as WatchEvent;
    }
  },

  /** A new empty file of the application's own; it needs no scope while this document lives. */
  tempFile: (options: Cancelable = {}): Promise<string> => call<string>('fs.tempFile', null, options),

  /** A new empty folder of the application's own; it needs no scope while this document lives. */
  tempDir: (options: Cancelable = {}): Promise<string> => call<string>('fs.tempDir', null, options),
};
