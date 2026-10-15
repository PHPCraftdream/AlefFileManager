// SPDX-License-Identifier: MIT OR Apache-2.0
import type { MenuItem } from '../../types/index.ts';
import { AlefError } from '../core/errors.ts';
import { on } from '../core/events.ts';
import { call } from '../core/transport.ts';
import type { Cancelable, Unlisten } from '../core/events.ts';

// Manual wire DTO: substitution preserves the shape but never emits native events.
interface Registration {
  id: string;
  owner: number;
  token: string;
}

export interface TrayOptions {
  /** PNG: 1..256 KiB, dimensions 1..256 pixels on each side. */
  icon: Uint8Array<ArrayBuffer>;
  /** At most 128 UTF-8 bytes without control characters. */
  tooltip?: string;
  /** Native menu tree; accelerators are not supported in tray menus. */
  menu?: MenuItem[];
}

export interface TrayEvents {
  click: { tray: Tray; button: 'left' | 'right' | 'middle' };
  'double-click': { tray: Tray; button: 'left' | 'right' | 'middle' };
  'right-click': { tray: Tray; button: 'left' | 'right' | 'middle' };
  'menu-click': { tray: Tray; id: string };
}

/** A document-owned native tray. Event subscriptions are per instance, not module-wide. */
export class Tray {
  readonly id: string;
  readonly #owner: number;
  readonly #token: string;
  #destroyed = false;

  constructor(reply: Registration) {
    this.id = reply.id;
    this.#owner = reply.owner;
    this.#token = reply.token;
  }

  async setIcon(icon: Uint8Array<ArrayBuffer>, options: Cancelable = {}): Promise<void> {
    await call<void>('tray.setIcon', { id: this.id }, { body: icon, signal: options.signal });
  }

  async setTooltip(tooltip: string, options: Cancelable = {}): Promise<void> {
    await call<void>('tray.setTooltip', { id: this.id, tooltip }, options);
  }

  async setMenu(menu: MenuItem[], options: Cancelable = {}): Promise<void> {
    await call<void>('tray.setMenu', { id: this.id, menu }, options);
  }

  /** A failed destroy leaves the instance live so the caller can retry. */
  async destroy(options: Cancelable = {}): Promise<void> {
    await call<void>('tray.destroy', { id: this.id }, options);
    this.#destroyed = true;
  }

  async on<K extends keyof TrayEvents>(
    event: K,
    callback: (payload: TrayEvents[K]) => void,
    options: Cancelable = {},
  ): Promise<Unlisten> {
    if (!['click', 'double-click', 'right-click', 'menu-click'].includes(event)) {
      throw new AlefError('INVALID_ARGUMENT', 'Unknown tray event.');
    }
    return on<unknown>(`runtime.tray.${event}`, payload => {
      if (this.#destroyed || typeof payload !== 'object' || payload === null) return;
      const { owner, token, button, id } = payload as {
        owner?: unknown; token?: unknown; button?: unknown; id?: unknown;
      };
      if (owner !== this.#owner || token !== this.#token) return;
      if (event === 'menu-click') {
        if (typeof id === 'string' && id.length > 0) callback({ tray: this, id } as unknown as TrayEvents[K]);
      } else if (button === 'left' || button === 'right' || button === 'middle') {
        callback({ tray: this, button } as unknown as TrayEvents[K]);
      }
    }, options);
  }
}

const asyncDispose = (Symbol as unknown as { asyncDispose?: symbol }).asyncDispose;
if (typeof asyncDispose === 'symbol') {
  Object.defineProperty(Tray.prototype, asyncDispose, {
    value(this: Tray): Promise<void> {
      return this.destroy();
    },
  });
}

export const tray = {
  /** Needs `permissions.tray`; Windows/macOS native support, Linux rejects `NOT_AVAILABLE`. */
  create: async ({ icon, tooltip, menu }: TrayOptions, options: Cancelable = {}): Promise<Tray> =>
    new Tray(await call<Registration>('tray.create', { tooltip, menu }, { body: icon, signal: options.signal })),
};
