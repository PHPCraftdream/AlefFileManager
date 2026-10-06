// SPDX-License-Identifier: MIT OR Apache-2.0
import type { AppInfo, ParsedArgs } from '../../types/index.ts';
import { call } from '../core/transport.ts';

export interface Cancelable {
  signal?: AbortSignal;
}

/** The running application: identity, command line, environment, lifetime. */
export const app = {
  /** `id`, `name` and `version` come from the manifest, `runtimeVersion` from the Alef runtime. */
  info: (options: Cancelable = {}): Promise<AppInfo> => call<AppInfo>('app.info', null, options),

  /** Asks the runtime to close the window and exit with `code` (0..255, default 0). */
  quit: (code?: number, options: Cancelable = {}): Promise<void> =>
    call<void>('app.quit', code === undefined ? {} : { code }, options),

  /** Starts a new instance with the same arguments and quits this one. */
  relaunch: (options: Cancelable = {}): Promise<void> => call<void>('app.relaunch', null, options),

  /** The command line, parsed by the `arguments` schema of the manifest. */
  args: (options: Cancelable = {}): Promise<ParsedArgs> => call<ParsedArgs>('app.args', null, options),

  /**
   * One environment variable (`undefined` when it is not set), or every variable of
   * `permissions.app.env` that is set. A name the manifest does not list is `PERMISSION_DENIED`.
   */
  env: (async (name?: string, options: Cancelable = {}): Promise<string | Record<string, string> | undefined> => {
    if (name === undefined) return await call<Record<string, string>>('app.envAll', null, options);
    return (await call<string | null>('app.env', { name }, options)) ?? undefined;
  }) as {
    (name: string, options?: Cancelable): Promise<string | undefined>;
    (name?: undefined, options?: Cancelable): Promise<Record<string, string>>;
  },

  /** The working directory of the process. */
  cwd: (options: Cancelable = {}): Promise<string> => call<string>('app.cwd', null, options),
};
