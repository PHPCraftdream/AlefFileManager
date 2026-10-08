// SPDX-License-Identifier: MIT OR Apache-2.0
import { crypto } from '../../../api/src/data/crypto.ts';
import { app } from '../../../api/src/desktop/app.ts';
import { cli } from '../../../api/src/system/cli.ts';
import { http } from '../../../api/src/net/http.ts';
import type { Streams } from './stdio.ts';
export const runtime = { cli, http, crypto };
/** The standard streams of a console utility (`console: true` in the manifest). */
export function consoleStreams(): Streams {
  return { input: app.stdin, output: app.stdout };
}
