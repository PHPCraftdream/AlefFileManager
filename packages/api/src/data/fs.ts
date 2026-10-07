// SPDX-License-Identifier: MIT OR Apache-2.0
import type { DirEntry, FileStat } from '../../types/index.ts';
import { call } from '../core/transport.ts';
import type { Cancelable } from '../desktop/app.ts';

const encoder = new TextEncoder();

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

  /** A new empty file of the application's own; it needs no scope while this document lives. */
  tempFile: (options: Cancelable = {}): Promise<string> => call<string>('fs.tempFile', null, options),

  /** A new empty folder of the application's own; it needs no scope while this document lives. */
  tempDir: (options: Cancelable = {}): Promise<string> => call<string>('fs.tempDir', null, options),
};
