// SPDX-License-Identifier: MIT OR Apache-2.0
import { AlefError } from '../core/errors.ts';
import { on } from '../core/events.ts';
import { call } from '../core/transport.ts';
import type { Cancelable, Unlisten } from '../core/events.ts';

// Manual wire DTO: native event identity is separate from the resource handle.
interface Registration {
  id: string;
  owner: number;
  token: number;
}

let createShortcut: (reply: Registration) => Shortcut;

/** A document-owned global shortcut; a substituted registration never emits events. */
export class Shortcut {
  readonly id: string;
  readonly #owner: number;
  readonly #token: number;
  #registered = true;

  private constructor(reply: Registration) {
    this.id = reply.id;
    this.#owner = reply.owner;
    this.#token = reply.token;
    // Registered on first construction, not at module level: importing the module must do nothing.
    const asyncDispose = (Symbol as unknown as { asyncDispose?: symbol }).asyncDispose;
    if (typeof asyncDispose === 'symbol' && !Object.hasOwn(Shortcut.prototype, asyncDispose)) {
      Object.defineProperty(Shortcut.prototype, asyncDispose, {
        value(this: Shortcut): Promise<void> {
          return this.unregister();
        },
      });
    }
  }

  static {
    createShortcut = reply => new Shortcut(reply);
  }

  /** Resolves once the shared event subscription is ready, with a function that unsubscribes. */
  async on(event: 'pressed', callback: () => void, options: Cancelable = {}): Promise<Unlisten> {
    if (event !== 'pressed') throw new AlefError('INVALID_ARGUMENT', 'Unknown shortcut event.');
    options.signal?.throwIfAborted();
    return on<unknown>('runtime.shortcut.pressed', payload => {
      if (!this.#registered || typeof payload !== 'object' || payload === null) return;
      const { owner, token } = payload as { owner?: unknown; token?: unknown };
      if (owner === this.#owner && token === this.#token) callback();
    }, options);
  }

  /** Unregisters by resource handle, not by the native event token. */
  async unregister(options: Cancelable = {}): Promise<void> {
    await call<void>('shortcut.unregister', { id: this.id }, options);
    this.#registered = false;
  }
}

export const shortcut = {
  /** Needs `permissions.shortcut.global`; the registration belongs to this document. */
  register: async (accelerator: string, options: Cancelable = {}): Promise<Shortcut> =>
    createShortcut(await call<Registration>('shortcut.register', { accelerator }, options)),
};
