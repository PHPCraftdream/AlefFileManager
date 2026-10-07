// SPDX-License-Identifier: MIT OR Apache-2.0
import { call } from '../core/transport.ts';
import type { Cancelable } from '../desktop/app.ts';

type Secret = Uint8Array<ArrayBuffer> | string;

const encoder = new TextEncoder();
const decoder = new TextDecoder();
const bytesOf = (secret: Secret): Uint8Array<ArrayBuffer> => (typeof secret === 'string' ? encoder.encode(secret) : secret);

/**
 * Passwords, tokens and keys in the credential store of the system (Windows Credential Manager, macOS
 * Keychain, the Secret Service of Linux). They are the application's own: another application does not
 * find what this one keeps under the same names. The manifest asks for the right with `secrets: true`;
 * a user may let an application have a stand-in, which keeps its secrets in memory until it quits.
 * The service and the account have from 1 to 128 bytes, without control characters; a secret has from
 * 1 to 1024 bytes (a string is its UTF-8).
 */
export const secrets = {
  /** The secret kept under `service` and `account`; `null` when there is none. */
  get: (service: string, account: string, options: Cancelable = {}): Promise<Uint8Array<ArrayBuffer> | null> =>
    call<Uint8Array<ArrayBuffer> | null>('secrets.get', { service, account }, options),

  /** As `get`, for a secret that is text (UTF-8). */
  getText: async (service: string, account: string, options: Cancelable = {}): Promise<string | null> => {
    const secret = await call<Uint8Array<ArrayBuffer> | null>('secrets.get', { service, account }, options);
    return secret === null ? null : decoder.decode(secret);
  },

  /** Keeps `secret`, replacing what was kept under the same `service` and `account`. */
  set: (service: string, account: string, secret: Secret, options: Cancelable = {}): Promise<void> =>
    call<void>('secrets.set', { service, account }, { ...options, body: bytesOf(secret) }),

  /** Forgets the secret; whether there was one. */
  delete: async (service: string, account: string, options: Cancelable = {}): Promise<boolean> => {
    const { deleted } = await call<{ deleted: boolean }>('secrets.delete', { service, account }, options);
    return deleted;
  },
};
