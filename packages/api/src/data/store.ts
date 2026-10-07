// SPDX-License-Identifier: MIT OR Apache-2.0
import { AlefError } from '../core/errors.ts';
import { call } from '../core/transport.ts';
import type { Cancelable } from '../desktop/app.ts';

/**
 * One area of the store of the application: values (anything JSON can hold) by key, kept between
 * runs. The store is the application's own and needs no right.
 */
export class Store {
  /** The name of the area; `undefined` for the default one. */
  readonly area: string | undefined;

  constructor(area?: string) {
    this.area = area;
  }

  /** The value of `key`; `undefined` when nothing is stored (a stored `null` comes back as `null`). */
  async get<T = unknown>(key: string, options: Cancelable = {}): Promise<T | undefined> {
    const reply = await call<{ value?: T }>('store.get', { area: this.area, key }, options);
    return 'value' in reply ? reply.value : undefined;
  }

  /** Stores `value` (up to 256 KiB as JSON; a bigger one is for `fs` or `sqlite`) under `key`; `undefined` is not a value: `delete` the key. */
  async set(key: string, value: unknown, options: Cancelable = {}): Promise<void> {
    if (value === undefined) {
      throw new AlefError('INVALID_ARGUMENT', 'undefined cannot be stored: delete the key instead');
    }
    await call<void>('store.set', { area: this.area, key, value }, options);
  }

  /** Removes `key`; a key that is not there is not an error. */
  delete(key: string, options: Cancelable = {}): Promise<void> {
    return call<void>('store.delete', { area: this.area, key }, options);
  }

  /** The keys, in order; only those that start with `prefix` when it is given. */
  keys(prefix?: string, options: Cancelable = {}): Promise<string[]> {
    return call<string[]>('store.keys', { area: this.area, prefix }, options);
  }

  /** Waits until everything written is on the disk (a written value is safe from a crash of the program without it). */
  flush(options: Cancelable = {}): Promise<void> {
    return call<void>('store.flush', null, options);
  }
}

const standard = new Store();

/** The default area of the store, and `open` for the others. */
export const store = {
  get: <T = unknown>(key: string, options: Cancelable = {}): Promise<T | undefined> => standard.get<T>(key, options),
  set: (key: string, value: unknown, options: Cancelable = {}): Promise<void> => standard.set(key, value, options),
  delete: (key: string, options: Cancelable = {}): Promise<void> => standard.delete(key, options),
  keys: (prefix?: string, options: Cancelable = {}): Promise<string[]> => standard.keys(prefix, options),
  flush: (options: Cancelable = {}): Promise<void> => standard.flush(options),

  /** A separate area (letters, digits, `_` and `-`, up to 64): its keys are its own. */
  open: async (name: string, options: Cancelable = {}): Promise<Store> => {
    await call<void>('store.open', { area: name }, options);
    return new Store(name);
  },
};
