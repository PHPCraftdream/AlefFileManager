// SPDX-License-Identifier: MIT OR Apache-2.0
// Release planner: one consistent version across the publishable npm packages, matched to the
// tag in real mode; prints the publish order (platform packages first) and the build matrix.
//   node scripts/release/plan.mjs --event push --ref refs/tags/v0.1.0
// Pure logic lives in exports; the CLI sits at the bottom.

import { readdirSync, readFileSync, appendFileSync, existsSync } from 'node:fs';
import { fileURLToPath } from 'node:url';
import path from 'node:path';

// packages/runtime/src/platforms.mjs uses `ciName` as the join key; the suffix here is the
// matching npm package suffix (@alef-runtime/runtime-<suffix>).
export const SUFFIX_BY_CI_NAME = {
  'windows-x64': 'win32-x64',
  'macos-arm64': 'darwin-arm64',
  'macos-x64': 'darwin-x64',
  'linux-x64': 'linux-x64',
};

export const PUBLISH_TAIL = ['@alef-runtime/runtime', '@alef-runtime/api', '@alef-runtime/mcp', '@alef-runtime/tools', 'create-alef-runtime'];

// The build matrix has one source: the platforms table in scripts/ci/affected.json.
export function loadPlatforms(root) {
  const data = JSON.parse(readFileSync(path.join(root, 'scripts', 'ci', 'affected.json'), 'utf8'));
  return data.platforms;
}

export function platformPackageNames(platforms) {
  return platforms.map(p => {
    const suffix = SUFFIX_BY_CI_NAME[p.name];
    if (!suffix) throw new Error(`no platform suffix for CI name: ${p.name}`);
    return `@alef-runtime/runtime-${suffix}`;
  });
}

export function collectPackages(root) {
  const packagesDir = path.join(root, 'packages');
  const packages = [];
  if (!existsSync(packagesDir)) return packages;
  for (const entry of readdirSync(packagesDir, { withFileTypes: true })) {
    if (!entry.isDirectory()) continue;
    const manifest = path.join(packagesDir, entry.name, 'package.json');
    if (!existsSync(manifest)) continue;
    const pkg = JSON.parse(readFileSync(manifest, 'utf8'));
    // optionalDependencies is kept so checkPlatformNames can verify the platform packages.
    const item = { name: pkg.name, version: pkg.version, dir: `packages/${entry.name}` };
    if (pkg.optionalDependencies) item.optionalDependencies = pkg.optionalDependencies;
    packages.push(item);
  }
  return packages;
}

export function checkVersions(packages) {
  const expected = [...packages].sort((a, b) => a.name.localeCompare(b.name))[0]?.version;
  for (const pkg of [...packages].sort((a, b) => a.name.localeCompare(b.name))) {
    if (pkg.version !== expected) throw new Error(`version drift: ${pkg.name} has ${pkg.version}, expected ${expected}`);
  }
  return expected;
}

export function checkPlatformNames(names, packages) {
  const runtime = packages.find(pkg => pkg.name === '@alef-runtime/runtime');
  if (!runtime) throw new Error('@alef-runtime/runtime is not in packages');
  const declared = new Set(Object.keys(runtime.optionalDependencies ?? {}));
  for (const name of names) {
    if (!declared.has(name)) throw new Error(`platform package ${name} is missing from @alef-runtime/runtime optionalDependencies`);
  }
}

export function checkTailPackages(order, packages) {
  const known = new Set(packages.map(pkg => pkg.name));
  for (const name of order) {
    if (name.startsWith('@alef-runtime/runtime-')) continue;
    if (!known.has(name)) throw new Error(`${name} is not in packages`);
  }
}

export function planRelease({ platforms, packages, tag, real }) {
  const version = checkVersions(packages);
  checkPlatformNames(platformPackageNames(platforms), packages);
  const expected = `v${version}`;
  if (real && tag !== expected) throw new Error(`tag ${tag} does not match version ${version} (expected ${expected})`);
  const order = [...platformPackageNames(platforms), ...PUBLISH_TAIL];
  checkTailPackages(order, packages);
  return { version, real, order, matrix: { include: platforms } };
}

function parseArgs(argv) {
  const args = {};
  for (let i = 0; i < argv.length; i++) {
    const flag = argv[i];
    if (!flag.startsWith('--')) continue;
    const next = argv[i + 1];
    if (next === undefined || next.startsWith('--')) {
      args[flag] = true;
    } else {
      args[flag] = next;
      i++;
    }
  }
  return args;
}

function main() {
  const root = fileURLToPath(new URL('../../', import.meta.url));
  const args = parseArgs(process.argv.slice(2));
  const event = args['--event'] ?? process.env.GITHUB_EVENT_NAME ?? '';
  const ref = args['--ref'] ?? process.env.GITHUB_REF ?? '';
  const real = event === 'push' && /^refs\/tags\/v/.test(ref);
  const tag = real ? ref.split('/').pop() : '';

  const platforms = loadPlatforms(root);
  const packages = collectPackages(root);
  let result;
  try {
    result = planRelease({ platforms, packages, tag, real });
  } catch (error) {
    console.error(`release plan: ${error.message}`);
    process.exitCode = 1;
    return;
  }
  console.log(`real=${result.real}`);
  console.log(`version=${result.version}`);
  console.log(`order=${result.order.join(',')}`);
  console.log(`matrix=${JSON.stringify(result.matrix)}`);
  if (process.env.GITHUB_OUTPUT) {
    appendFileSync(process.env.GITHUB_OUTPUT, [
      `real=${result.real}`,
      `version=${result.version}`,
      `order=${result.order.join(',')}`,
      `matrix=${JSON.stringify(result.matrix)}`,
    ].join('\n') + '\n');
  }
}

const isMain = process.argv[1] && process.argv[1] === fileURLToPath(import.meta.url);
if (isMain) main();
