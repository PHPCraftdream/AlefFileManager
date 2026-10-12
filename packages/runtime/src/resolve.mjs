// SPDX-License-Identifier: MIT OR Apache-2.0
import { statSync } from 'node:fs';
import { createRequire } from 'node:module';
import { dirname, join, resolve } from 'node:path';
import { PLATFORMS, packageName } from './platforms.mjs';

const require = createRequire(import.meta.url);

function regularFile(path, detail, fix) {
  try {
    // Follow filesystem aliases, including symlinked parents and Windows short names.
    // No lexical comparison against realpath: spelling is not file identity.
    if (statSync(path).isFile()) return path;
  } catch (cause) {
    throw new Error(`${detail}: ${path} (${cause.code ?? cause.message}). ${fix}`, { cause });
  }
  throw new Error(`${detail}: ${path} is not a regular file. ${fix}`);
}

export function resolveRuntimePath({ platform = process.platform, arch = process.arch, env = process.env } = {}) {
  if (env.ALEF_BIN !== undefined) {
    const fix = 'Set ALEF_BIN to an existing runtime file, or unset it to use the installed platform package.';
    if (typeof env.ALEF_BIN !== 'string' || env.ALEF_BIN.length === 0 || env.ALEF_BIN.includes('\0')) {
      throw new Error(`Invalid ALEF_BIN: expected a nonempty file path. ${fix}`);
    }
    return regularFile(resolve(env.ALEF_BIN), 'Invalid ALEF_BIN', fix);
  }
  const entry = PLATFORMS.find(item => item.platform === platform && item.arch === arch);
  if (!entry) {
    throw new Error(`Unsupported Alef runtime platform ${platform}/${arch}; no platform package is available. Supported packages: ${PLATFORMS.map(packageName).join(', ')}. Use a supported platform or set ALEF_BIN to an existing runtime file.`);
  }
  const name = packageName(entry);
  const fix = `Install ${name} with optional dependencies enabled (npm install --include=optional); do not use --omit=optional, or set ALEF_BIN to an existing runtime file.`;
  let manifest;
  try {
    manifest = require.resolve(`${name}/package.json`);
  } catch (cause) {
    throw new Error(`Missing Alef runtime package ${name} for ${platform}/${arch}. ${fix}`, { cause });
  }
  return regularFile(join(dirname(manifest), 'bin', entry.binary), `Missing or invalid Alef runtime binary for ${platform}/${arch} in ${name}`, fix);
}
