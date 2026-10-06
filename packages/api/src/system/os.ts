// SPDX-License-Identifier: MIT OR Apache-2.0
import type { OsInfo, Theme } from '../../types/index.ts';
import { on } from '../core/events.ts';
import { call } from '../core/transport.ts';
import type { Cancelable } from '../desktop/app.ts';

export type OsEvent = 'theme-changed';

/** The machine: platform facts and the desktop theme. */
export const os = {
  /** `platform` and `arch` are Rust `target_os`/`target_arch` names (`windows`, `x86_64`, ...). */
  info: (options: Cancelable = {}): Promise<OsInfo> => call<OsInfo>('os.info', null, options),

  /** The desktop colour scheme; `light` where the platform does not report one. */
  theme: (options: Cancelable = {}): Promise<Theme> => call<Theme>('os.theme', null, options),

  /** Resolves with the function that unsubscribes once the subscription is in place. */
  on: (event: OsEvent, handler: (theme: Theme) => void, options: Cancelable = {}): Promise<() => void> =>
    on<{ theme: Theme }>(`os.${event}`, payload => handler(payload.theme), options),
};
