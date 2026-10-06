// SPDX-License-Identifier: MIT OR Apache-2.0
import { call } from '../core/transport.ts';
import type { Cancelable } from '../desktop/app.ts';

export interface NotificationOptions {
  /** Up to 128 characters, no line breaks. */
  title: string;
  /** Up to 1024 characters; line breaks and tabs are fine. */
  body?: string;
  /** The path of a file the document may read. */
  icon?: string;
}

/** Messages the desktop shows outside the window. */
export const notification = {
  /**
   * Shows a notification. Rejects with `NOT_AVAILABLE` where the desktop cannot show one for this
   * application: Windows and macOS show notifications only for an application with an identity (an
   * AppUserModelID, a signed bundle), which an application run from a folder does not have, and
   * Linux needs `notify-send`. `PERMISSION_DENIED` for an icon the document may not read.
   */
  show: (options: NotificationOptions, cancel: Cancelable = {}): Promise<void> =>
    call<void>('notification.show', options, cancel),
};
