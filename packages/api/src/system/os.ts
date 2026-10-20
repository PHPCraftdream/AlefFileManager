// SPDX-License-Identifier: MIT OR Apache-2.0
import type { OsInfo, Theme } from '../../types/index.ts';
import { on, type Cancelable, type Unlisten } from '../core/events.ts';
import { call } from '../core/transport.ts';

export type OsEvent = 'theme-changed' | 'suspend' | 'resume';

interface OsSubscribe {
  (event: 'theme-changed', handler: (theme: Theme) => void, options?: Cancelable): Promise<Unlisten>;
  (event: 'suspend' | 'resume', handler: () => void, options?: Cancelable): Promise<Unlisten>;
}

/** `platform` and `arch` are Rust `target_os`/`target_arch` names (`windows`, `x86_64`, ...). */
const info = (options: Cancelable = {}): Promise<OsInfo> => call<OsInfo>('os.info', null, options);

/** The desktop colour scheme; `light` where the platform does not report one. */
const theme = (options: Cancelable = {}): Promise<Theme> => call<Theme>('os.theme', null, options);

/**
 * Resolves with the function that unsubscribes once the subscription is in place. For
 * `suspend`/`resume` the event subscription is made first and the process-wide watcher is
 * started after it, so events raised after `os.on` has resolved are not missed; if the
 * watcher cannot start, the subscription is undone and the error rethrown.
 */
const subscribe: OsSubscribe = async (event, handler, options = {}) => {
  if (event === 'theme-changed') {
    return on<{ theme: Theme }>(
      'os.theme-changed',
      payload => (handler as (theme: Theme) => void)(payload.theme),
      options,
    );
  }
  const unlisten = await on<void>(`os.${event}`, handler as () => void, options);
  try {
    await call('os.watchPower', null, options);
  } catch (error) {
    unlisten();
    throw error;
  }
  return unlisten;
};

/** The machine: platform facts, the desktop theme, and the power state. */
export const os = { info, theme, on: subscribe };
