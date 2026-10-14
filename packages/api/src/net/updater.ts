// SPDX-License-Identifier: MIT OR Apache-2.0
import type { UpdaterCheckReply } from '../../types/modules.ts';
import { openReadable } from '../core/stream.ts';
import { call } from '../core/transport.ts';

export interface UpdaterDownloadOptions {
  onProgress?: (progress: { received: number; total: number | null }) => void;
  signal?: AbortSignal;
}

export const updater = {
  /** Checks the configured signed-update feed. */
  check: () => call<UpdaterCheckReply>('updater.check', {}),

  /** Downloads and verifies the offered update. Resolves only after the stream completes successfully. */
  download: async ({ onProgress, signal }: UpdaterDownloadOptions = {}): Promise<void> => {
    const { stream, resource } = await call<{ stream: number; resource: number }>('updater.download', {}, { signal });
    let completed = false;
    try {
      for await (const frame of await openReadable(stream, { signal })) {
        if (frame.kind !== 'json') continue;
        const value = frame.value as { received?: number; total?: number | null; done?: boolean };
        if (typeof value.received === 'number') {
          onProgress?.({ received: value.received, total: value.total ?? null });
        }
        if (value.done === true) completed = true;
      }
      if (!completed) throw new Error('Updater download stream ended before completion.');
      signal?.throwIfAborted();
    } finally {
      try {
        await call('updater.close', { id: resource });
      } catch {
        // Best effort: preserve any download, callback, or abort error.
      }
    }
  },
};
