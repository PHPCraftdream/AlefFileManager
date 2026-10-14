// SPDX-License-Identifier: MIT OR Apache-2.0
// Stages one package outside its private workspace copy and runs npm publish (real or --dry-run).
//   node scripts/release/publish-package.mjs <package dir> <version> [--dry-run]

import { cpSync, copyFileSync, existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { statSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import path from 'node:path';

// A manifest without `files` packs everything npm's defaults allow; mirror that by staging the
// whole directory (node_modules and .git excluded), so create-alef-runtime ships bin, src and templates.
export function filesToCopy(manifest) {
  return Array.isArray(manifest.files) && manifest.files.length > 0 ? manifest.files : null;
}

// Copies the package contents into a fresh staging directory, mirroring npm's `files` semantics.
export function copyInto(dir, staging, manifest) {
  const files = filesToCopy(manifest);
  if (files) {
    for (const entry of files) {
      const source = path.join(dir, entry);
      const target = path.join(staging, entry);
      if (!existsSync(source)) continue;
      // a `files` entry is either a single file or a directory copied wholesale
      if (statSync(source).isDirectory()) {
        cpSync(source, target, { recursive: true });
      } else {
        copyFileSync(source, target);
      }
    }
    return;
  }
  // staging is fresh (rmSync above); skip the workspace noise npm would skip anyway
  const sourceRoot = path.resolve(dir);
  cpSync(sourceRoot, staging, { recursive: true, filter: src => !/(^|[\\/])(node_modules|\.git)([\\/]|$)/.test(src) });
}

function run(args) {
  const dry = args.includes('--dry-run');
  const positional = args.filter(a => a !== '--dry-run');
  if (positional.length !== 2) {
    console.error('Usage: publish-package.mjs <package dir> <version> [--dry-run]');
    process.exitCode = 1;
    return;
  }
  const [dir, version] = positional;
  const repoRoot = fileURLToPath(new URL('../../', import.meta.url));
  const manifest = JSON.parse(readFileSync(path.join(dir, 'package.json'), 'utf8'));
  const staging = path.join(repoRoot, 'backend', 'target', 'release-npm', manifest.name.replaceAll('/', '-'));
  rmSync(staging, { recursive: true, force: true });
  mkdirSync(staging, { recursive: true });
  copyInto(dir, staging, manifest);
  // The private workspace copy is never published; npm refuses private packages, so the
  // staging manifest drops `private`.
  const manifest2 = structuredClone(manifest);
  manifest2.version = version;
  delete manifest2.private;
  // The generator emits platform packages at this version; the placeholder in the meta
  // package resolves to it at release time.
  for (const [name, value] of Object.entries(manifest2.optionalDependencies ?? {})) {
    if (value === 'workspace-version-placeholder') manifest2.optionalDependencies[name] = version;
  }
  writeFileSync(path.join(staging, 'package.json'), `${JSON.stringify(manifest2, null, 2)}\n`);
  const npmArgs = dry
    ? ['publish', '--dry-run', '--access', 'public']
    : ['publish', '--access', 'public', '--provenance'];
  const result = spawnSync('npm', npmArgs, {
    cwd: staging, stdio: 'inherit', shell: process.platform === 'win32',
    env: { ...process.env, NODE_AUTH_TOKEN: process.env.NODE_AUTH_TOKEN ?? '' },
  });
  if (result.status !== 0) {
    console.error(`publish-package: npm publish failed for ${manifest.name}`);
    process.exitCode = result.status ?? 1;
    return;
  }
  console.log(`publish-package: ${dry ? 'dry-run' : 'published'} ${manifest.name}@${version}`);
}

const isMain = process.argv[1] && process.argv[1] === fileURLToPath(import.meta.url);
if (isMain) {
  try {
    run(process.argv.slice(2));
  } catch (error) {
    console.error(`publish-package: ${error.message}`);
    process.exitCode = 1;
  }
}
