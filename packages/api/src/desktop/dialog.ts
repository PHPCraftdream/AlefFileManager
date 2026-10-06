// SPDX-License-Identifier: MIT OR Apache-2.0
import type { ConfirmOptions, MessageOptions, OpenOptions, SaveOptions } from '../../types/index.ts';
import { call } from '../core/transport.ts';
import type { Cancelable } from './app.ts';

/**
 * Native dialogs on top of the window of this document. The dialogs of one application take turns.
 * Aborting `signal` stops the wait, not the dialog: it stays until the user closes it.
 */
export const dialog = {
  /**
   * The paths the user picked, `[]` when the dialog was cancelled. What the user picks becomes
   * readable for this document until it unloads: a file, or a folder with everything below it
   * (`directory: true`).
   */
  open: (options: OpenOptions = {}, cancel: Cancelable = {}): Promise<string[]> =>
    call<string[]>('dialog.open', options, cancel),

  /** The path the user chose, `null` when cancelled. That one file becomes writable for this document. */
  save: (options: SaveOptions = {}, cancel: Cancelable = {}): Promise<string | null> =>
    call<string | null>('dialog.save', options, cancel),

  /** Shows the message and resolves when the user closes it. */
  message: (options: MessageOptions, cancel: Cancelable = {}): Promise<void> =>
    call<void>('dialog.message', options, cancel),

  /** `true` when the user pressed the confirming button, `false` for the other one or for closing it. */
  confirm: (options: ConfirmOptions, cancel: Cancelable = {}): Promise<boolean> =>
    call<boolean>('dialog.confirm', options, cancel),
};
