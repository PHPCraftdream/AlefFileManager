// SPDX-License-Identifier: MIT OR Apache-2.0
import { call } from '../core/transport.ts';
import type { Cancelable } from '../desktop/app.ts';

const encoder = new TextEncoder();
// A byte order mark at the start is part of the text on the clipboard: keep it.
const decoder = new TextDecoder('utf-8', { ignoreBOM: true });

/**
 * The clipboard. Reading needs `permissions.clipboard.read`, writing needs nothing. The clipboard
 * holds one thing at a time: writing text replaces an image and the other way round. Contents
 * travel as bytes, so their size is not limited by the size of a JSON call.
 */
export const clipboard = {
  /** The text on the clipboard; `''` when there is none. */
  readText: async (options: Cancelable = {}): Promise<string> =>
    decoder.decode(await call<Uint8Array>('clipboard.readText', null, options)),

  writeText: (text: string, options: Cancelable = {}): Promise<void> =>
    call<void>('clipboard.writeText', null, { ...options, body: encoder.encode(text) }),

  /** The HTML on the clipboard; `''` when there is none. */
  readHtml: async (options: Cancelable = {}): Promise<string> =>
    decoder.decode(await call<Uint8Array>('clipboard.readHtml', null, options)),

  writeHtml: (html: string, options: Cancelable = {}): Promise<void> =>
    call<void>('clipboard.writeHtml', null, { ...options, body: encoder.encode(html) }),

  /** The image on the clipboard as PNG, `null` when there is none. */
  readImage: (options: Cancelable = {}): Promise<Uint8Array | null> =>
    call<Uint8Array | null>('clipboard.readImage', null, options),

  /** `png` is a PNG of up to 8192 by 8192 pixels. */
  writeImage: (png: Uint8Array<ArrayBuffer>, options: Cancelable = {}): Promise<void> =>
    call<void>('clipboard.writeImage', null, { ...options, body: png }),
};
