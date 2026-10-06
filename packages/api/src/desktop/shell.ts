// SPDX-License-Identifier: MIT OR Apache-2.0
import { call } from '../core/transport.ts';
import type { Cancelable } from './app.ts';

/** Hands addresses and files to the desktop. */
export const shell = {
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
