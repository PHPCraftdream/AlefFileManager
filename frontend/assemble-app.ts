// SPDX-License-Identifier: MIT OR Apache-2.0
import { copyFileSync, mkdirSync } from 'node:fs';
import { join } from 'node:path';

/** Controls are copied explicitly, never through Rsbuild's public input. */
export function assembleControls(source: string, application: string): void {
  mkdirSync(application, { recursive: true });
  for (const name of ['alef.ktav', 'icon.png']) {
    copyFileSync(join(source, name), join(application, name));
  }
}
