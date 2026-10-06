// SPDX-License-Identifier: MIT OR Apache-2.0
import type { Cancelable } from '../desktop/app.ts';
import { call } from '../core/transport.ts';

const directory = (command: string) => (options: Cancelable = {}): Promise<string> => call<string>(command, null, options);

/**
 * Well-known directories (not created for you) and lexical path arithmetic in the separator style
 * of the platform. Nothing here reads the disk; to use a path see the `fs` permissions.
 */
export const path = {
  /** `<system data directory>/<app id>`. */
  appData: directory('path.appData'),
  appConfig: directory('path.appConfig'),
  appCache: directory('path.appCache'),
  temp: directory('path.temp'),
  home: directory('path.home'),
  documents: directory('path.documents'),
  downloads: directory('path.downloads'),
  desktop: directory('path.desktop'),
  /** The program being run. */
  executable: directory('path.executable'),

  /** Joins the parts and normalizes; an absolute part does not reset the path. */
  join: (...parts: string[]): Promise<string> => call<string>('path.join', { parts }),
  /** Collapses `.`, `..` and repeated separators; `.` when nothing is left. */
  normalize: (text: string): Promise<string> => call<string>('path.normalize', { path: text }),
  /** The parent of the normalized path: `.` for a bare name, the root for a root. */
  dirname: (text: string): Promise<string> => call<string>('path.dirname', { path: text }),
  /** The last name of the normalized path, empty for a root. */
  basename: (text: string): Promise<string> => call<string>('path.basename', { path: text }),
};
