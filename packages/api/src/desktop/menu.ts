// SPDX-License-Identifier: MIT OR Apache-2.0
import type { MenuItem } from '../../types/index.ts';
import { AlefError } from '../core/errors.ts';
import { on } from '../core/events.ts';
import { call } from '../core/transport.ts';
import type { Cancelable } from './app.ts';
import type { AppWindow, Unlisten } from './window.ts';

export type { MenuItem, MenuKind, MenuRole } from '../../types/index.ts';

/** Native menus owned by this document. Unsupported native features reject explicitly. */
export const menu = {
  setApplicationMenu: async (items: MenuItem[], options: Cancelable = {}): Promise<void> =>
    call<void>('menu.setApplicationMenu', { items }, options),

  setWindowMenu: async (window: AppWindow, items: MenuItem[], options: Cancelable = {}): Promise<void> =>
    call<void>('menu.setWindowMenu', { label: window.label, items }, options),

  /**
   * Opens a context menu and returns the id of the chosen item, or null when it was dismissed.
   * `x` and `y` go together, in logical pixels from the top-left corner of the window's content;
   * without them the menu opens at the pointer. Windows only: elsewhere it rejects `NOT_AVAILABLE`.
   */
  popup: async (items: MenuItem[], options: Cancelable & { x?: number; y?: number } = {}): Promise<string | null> =>
    call<string | null>('menu.popup', { items, x: options.x, y: options.y }, { signal: options.signal }),

  /** The runtime routes clicks to the owner document; all listeners share its event stream. */
  async on(event: 'click', callback: (payload: { id: string }) => void, options: Cancelable = {}): Promise<Unlisten> {
    if (event !== 'click') throw new AlefError('INVALID_ARGUMENT', 'Unknown menu event.');
    return on<unknown>('runtime.menu.clicked', payload => {
      if (typeof payload !== 'object' || payload === null) return;
      const { id } = payload as { id?: unknown };
      if (typeof id === 'string' && id.length > 0) callback({ id });
    }, options);
  },
};
