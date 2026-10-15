// SPDX-License-Identifier: MIT OR Apache-2.0
// Packs the built application directory with @alef-runtime/tools. A node script, not an npm
// one-liner: the key name must not depend on shell-specific env expansion (cmd.exe vs POSIX).
import { spawnSync } from 'node:child_process';
import { existsSync, mkdirSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import path from 'node:path';

const root = fileURLToPath(new URL('../../', import.meta.url));
const cli = path.join(root, 'packages', 'tools', 'bin', 'alef-runtime.mjs');
const application = path.join(root, 'frontend', 'app');
const out = process.argv[2] ?? path.join(root, 'dist', 'file-manager.alef');
const key = process.env.ALEF_RUNTIME_KEY;
if (!key || !/^[A-Za-z0-9_-][A-Za-z0-9_.-]*$/.test(key)) throw new Error('Set ALEF_RUNTIME_KEY to the signing key name (letters, digits, _ - . only).');
if (!existsSync(path.join(application, 'alef.ktav'))) throw new Error(`${application} has no alef.ktav: build the frontend first (npm run build:frontend).`);
mkdirSync(path.dirname(path.resolve(out)), { recursive: true });
const result = spawnSync(process.execPath, [cli, 'pack', application, '--key', key, '--out', out], { stdio: 'inherit', windowsHide: true });
if (result.error) throw result.error;
process.exitCode = result.status ?? 1;
