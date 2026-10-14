// SPDX-License-Identifier: MIT OR Apache-2.0
import { chmodSync, constants, copyFileSync, mkdirSync, readFileSync, readdirSync, statSync, writeFileSync } from 'node:fs';
import { join, resolve } from 'node:path';
import { PLATFORMS, packageName } from '../src/platforms.mjs';

function generate(args) {
  if (args.length !== 4 || args.some(arg => !arg || arg.includes('\0'))) {
    throw new Error('Usage: make-platform-package.mjs <suffix> <binary path> <out dir> <version>');
  }
  const [suffix, input, output, version] = args;
  const entry = PLATFORMS.find(item => item.suffix === suffix);
  if (!entry) throw new Error(`Unsupported platform suffix: ${suffix}`);
  const number = '(?:0|[1-9][0-9]*)';
  const pre = '(?:0|[1-9][0-9]*|[0-9]*[A-Za-z-][0-9A-Za-z-]*)';
  const semver = new RegExp(`^${number}\\.${number}\\.${number}(?:-${pre}(?:\\.${pre})*)?(?:\\+[0-9A-Za-z-]+(?:\\.[0-9A-Za-z-]+)*)?$`);
  if (version.length > 256 || !semver.test(version)) throw new Error(`Invalid SemVer version: ${version}`);
  const source = resolve(input); const destination = resolve(output);
  let info;
  try { info = statSync(source); }
  catch (cause) { throw new Error(`Input binary is missing or unreadable: ${source}`, { cause }); }
  if (!info.isFile()) throw new Error(`Input binary is not a regular file: ${source}`);
  // Read licenses before creating any output; the source tree is the license authority.
  const licenses = ['LICENSE-MIT', 'LICENSE-APACHE'].map(name => [name, readFileSync(new URL(`../../../${name}`, import.meta.url))]);
  mkdirSync(destination, { recursive: true });
  if (readdirSync(destination).length !== 0) throw new Error(`Output directory is not empty: ${destination}; refusing to overwrite`);
  mkdirSync(join(destination, 'bin'));
  const binary = join(destination, 'bin', entry.binary);
  copyFileSync(source, binary, constants.COPYFILE_EXCL);
  if (entry.platform !== 'win32') chmodSync(binary, 0o755);
  const manifest = {
    name: packageName(entry), version,
    description: `Alef runtime binary for ${entry.platform}/${entry.arch}`,
    license: 'MIT OR Apache-2.0',
    os: [entry.platform], cpu: [entry.arch],
    bin: { alef: `./bin/${entry.binary}` },
    files: ['bin', 'README.md', 'LICENSE-MIT', 'LICENSE-APACHE'],
  };
  writeFileSync(join(destination, 'package.json'), `${JSON.stringify(manifest, null, 2)}\n`, { flag: 'wx' });
  writeFileSync(join(destination, 'README.md'), `# ${manifest.name}\n\nAlef runtime ${version} for ${entry.platform}/${entry.arch}.\n\nInstalled as an optional dependency of @alef-runtime/runtime. Use that meta-package to select the binary, or set ALEF_BIN to an existing runtime file. Install with optional dependencies enabled (npm install --include=optional); do not use --omit=optional.\n\nLicense: MIT OR Apache-2.0; see LICENSE-MIT and LICENSE-APACHE.\n`, { flag: 'wx' });
  for (const [name, bytes] of licenses) writeFileSync(join(destination, name), bytes, { flag: 'wx' });
}

try {
  generate(process.argv.slice(2));
} catch (error) {
  console.error(`make-platform-package: ${error.message}`);
  process.exitCode = 1;
}
