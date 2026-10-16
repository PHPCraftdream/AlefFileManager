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
      // dist/ of packages/api is a build artifact: staging a manifest without its implementation
      // would publish a package that resolves to nothing, so a missing entry is an error, not a skip.
      if (!existsSync(source)) throw new Error(`${manifest.name}: "files" entry "${entry}" is missing (build the package first)`);
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

// npm runs these from the staging directory; when only the built `files` were staged, the sources
// they build from are not there, so a staged package carries none of them.
const BUILD_LIFECYCLE = ['prepare', 'prepack', 'prepublish', 'prepublishOnly', 'postpack'];

// The manifest written into staging: the release version, never `private`, the runtime placeholder
// resolved, and no build lifecycle when the staged tree is the built `files` only.
export function stagedManifest(manifest, version) {
  const staged = structuredClone(manifest);
  staged.version = version;
  delete staged.private;
  // The generator emits platform packages at this version; the placeholder in the meta
  // package resolves to it at release time.
  for (const [name, value] of Object.entries(staged.optionalDependencies ?? {})) {
    if (value === 'workspace-version-placeholder') staged.optionalDependencies[name] = version;
  }
  if (filesToCopy(manifest) && staged.scripts) {
    for (const hook of BUILD_LIFECYCLE) delete staged.scripts[hook];
    if (Object.keys(staged.scripts).length === 0) delete staged.scripts;
  }
  return staged;
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
  // npm refuses private packages: the private workspace copy is published through the staged manifest.
  writeFileSync(path.join(staging, 'package.json'), `${JSON.stringify(stagedManifest(manifest, version), null, 2)}\n`);
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
