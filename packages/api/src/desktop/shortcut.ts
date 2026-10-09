// SPDX-License-Identifier: MIT OR Apache-2.0
import { AlefError } from '../core/errors.ts';
import { on } from '../core/events.ts';
import { call } from '../core/transport.ts';
import type { Cancelable } from './app.ts';
import type { Unlisten } from './window.ts';

// Manual wire DTO: native event identity is separate from the resource handle.
interface Registration {
  id: string;
  owner: number;
  token: number | null;
}

/** A document-owned global shortcut; a substituted registration never emits events. */
export class Shortcut {
  readonly id: string;
  readonly #owner: number;
  readonly #token: number | null;
  #registered = true;

  constructor(reply: Registration) {
    this.id = reply.id;
    this.#owner = reply.owner;
    this.#token = reply.token;
  }

  /** Resolves once the shared event subscription is ready, with a function that unsubscribes. */
  async on(event: 'pressed', callback: () => void, options: Cancelable = {}): Promise<Unlisten> {
    if (event !== 'pressed') throw new AlefError('INVALID_ARGUMENT', 'Unknown shortcut event.');
    options.signal?.throwIfAborted();
    if (this.#token === null) return () => {};
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

const asyncDispose = (Symbol as unknown as { asyncDispose?: symbol }).asyncDispose;
if (typeof asyncDispose === 'symbol') {
  Object.defineProperty(Shortcut.prototype, asyncDispose, {
    value(this: Shortcut): Promise<void> {
      return this.unregister();
    },
  });
}

export const shortcut = {
  /** Needs `permissions.shortcut.global`; the registration belongs to this document. */
  register: async (accelerator: string, options: Cancelable = {}): Promise<Shortcut> =>
    new Shortcut(await call<Registration>('shortcut.register', { accelerator }, options)),
};
