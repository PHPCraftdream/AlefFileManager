// SPDX-License-Identifier: MIT OR Apache-2.0
import { decodeBase64 } from '../core/base64.ts';
import { AlefError } from '../core/errors.ts';
import { call } from '../core/transport.ts';
import type { Cancelable } from './app.ts';

/** Exactly one target selector is accepted. */
export type FileIconTarget =
  | { path: string; extension?: never; mime?: never; kind?: never }
  | { extension: string; path?: never; mime?: never; kind?: never }
  | { mime: string; path?: never; extension?: never; kind?: never }
  | { kind: 'folder' | 'file' | 'drive'; path?: never; extension?: never; mime?: never };

export type FileIconSize = 16 | 24 | 32 | 48 | 64 | 128 | 256;

export interface FileIconResult {
  png: Uint8Array;
  width: number;
  height: number;
}

export interface FileIconOptions extends Cancelable {
  /** PNG dimensions are requested size where supported; native sources may return a nearby size. Defaults to 32. */
  size?: FileIconSize;
}

const FILE_ICON_SIZES = new Set([16, 24, 32, 48, 64, 128, 256]);
const MIME_TOKEN = /^[a-z0-9!#$&^_.+-]{1,127}$/i;

function invalidIcon(message: string): AlefError {
  return new AlefError('INVALID_ARGUMENT', `shell.fileIcon: ${message}`);
}

function validateIconTarget(target: unknown): Record<string, string> {
  if (typeof target !== 'object' || target === null || Array.isArray(target)) throw invalidIcon('target must be an object');
  const record = target as Record<string, unknown>;
  const keys = Reflect.ownKeys(record);
  const recognized = ['path', 'extension', 'mime', 'kind'];
  const key = keys[0];
  if (keys.length !== 1 || typeof key !== 'string' || !recognized.includes(key)) throw invalidIcon('target must contain exactly one recognized selector');
  const value = record[key];
  if (typeof value !== 'string' || value.length === 0 || value.includes('\0')) throw invalidIcon(`${key} must be a nonempty string without NUL`);
  if (key === 'kind') {
    if (!['folder', 'file', 'drive'].includes(value)) throw invalidIcon('kind must be folder, file, or drive');
    return { kind: value };
  }
  if (key === 'extension') {
    const extension = value.trim().replace(/^\./, '').toLowerCase();
    if (!/^[a-z0-9_+-]{1,64}$/.test(extension)) throw invalidIcon('invalid extension');
    return { extension };
  }
  if (key === 'mime') {
    const mime = value.split(';', 1)[0].trim().toLowerCase();
    const parts = mime.split('/');
    if (parts.length !== 2 || !parts.every(part => MIME_TOKEN.test(part))) throw invalidIcon('invalid mime type');
    return { mime };
  }
  return { path: value };
}

/** Hands addresses and files to the desktop. */
export const shell = {
  /** Returns a native icon as PNG bytes. Paths must be readable by this document. */
  async fileIcon(
    target: FileIconTarget,
    options: FileIconOptions = {},
  ): Promise<FileIconResult> {
    const normalized = validateIconTarget(target);
    const size = options.size ?? 32;
    if (!FILE_ICON_SIZES.has(size)) throw invalidIcon('unsupported size');
    options.signal?.throwIfAborted();
    const reply = await call<{ png: string; width: number; height: number }>(
      'shell.fileIcon',
      { target: normalized, size },
      { signal: options.signal },
    );
    return { png: decodeBase64(reply.png), width: reply.width, height: reply.height };
  },

  /** Opens a web address in the browser; the address must be inside `permissions.shell.openExternal`. */
  openExternal: (url: string, options: Cancelable = {}): Promise<void> =>
    call<void>('shell.openExternal', { url }, options),

  /**
   * Opens a file or a folder with the program of the desktop. The path must be readable for this
   * document. A program, a script, a shortcut or an application is not opened: `PERMISSION_DENIED`.
   */
  openPath: (path: string, options: Cancelable = {}): Promise<void> =>
    call<void>('shell.openPath', { path }, options),

  /** Shows the file in the file manager (selected where the file manager can do it); the path must be readable. */
  showInFolder: (path: string, options: Cancelable = {}): Promise<void> =>
    call<void>('shell.showInFolder', { path }, options),

  /** Moves the file or folder to the trash; the path must be writable for this document. */
  trash: (path: string, options: Cancelable = {}): Promise<void> =>
    call<void>('shell.trash', { path }, options),
};
