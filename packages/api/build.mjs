// SPDX-License-Identifier: MIT OR Apache-2.0
// Build of @alef-runtime/api: ESM .js and .d.ts for every export subpath, into dist/.
// The "alef-sources" manifest field names, per subpath, the source it is built from: the
// same sources the segmentation guards check. Type-only entries emit a declaration only.
import { createRequire } from 'node:module';
import { readdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const pkgDir = dirname(fileURLToPath(import.meta.url));

// A declaration must never name a ".ts" source: consumers resolve the file it references as ".js".
// tsc usually rewrites already; this makes the guarantee independent of the compiler behavior.
export function rewriteDeclarationSpecifiers(text) {
  return text.replace(/(['"])(\.\.?\/[^'"]+)\.ts\1/g, '$1$2.js$1');
}

export function buildApi() {
  const manifest = JSON.parse(readFileSync(join(pkgDir, 'package.json'), 'utf8'));
  const sources = manifest['alef-sources'];
  const exportsMap = manifest.exports ?? {};
  if (typeof sources !== 'object' || sources === null) {
    throw new Error('package.json must carry the "alef-sources" map of subpath -> source');
  }
  const uncovered = Object.keys(exportsMap).filter(subpath => typeof sources[subpath] !== 'string');
  if (Object.keys(sources).length !== Object.keys(exportsMap).length || uncovered.length > 0) {
    throw new Error(`exports and "alef-sources" must cover the same subpaths; exports without a source: ${uncovered.join(', ')}`);
  }
  const entries = Object.entries(sources).map(([subpath, source]) => ({ subpath, source }));
  if (entries.length < 48) throw new Error(`expected every subpath of the package in exports, got ${entries.length}`);
  const runtimeEntries = Object.entries(exportsMap)
    .filter(([, value]) => typeof value === 'object' && value !== null && typeof value['import'] === 'string')
    .map(([subpath]) => ({ subpath, source: sources[subpath] }));

  // The compiler is the workspace typescript, resolved like tests/e2e/lib.mjs does.
  const tsc = join(dirname(createRequire(import.meta.url).resolve('typescript/package.json')), 'bin', 'tsc');
  const base = ['--ignoreConfig', '--strict', '--skipLibCheck', '--target', 'ES2022', '--module', 'ESNext',
    '--moduleResolution', 'bundler', '--rewriteRelativeImportExtensions', '--rootDir', pkgDir];
  const outDir = join(pkgDir, 'dist');

  rmSync(outDir, { recursive: true, force: true });

  // Runtime modules emit ESM; relative ".ts" specifiers are rewritten to ".js".
  const js = spawnSync(process.execPath, [tsc, ...base, '--outDir', outDir,
    ...runtimeEntries.map(entry => join(pkgDir, entry.source))], { timeout: 120000, encoding: 'utf8' });
  if (js.status !== 0) throw new Error(`tsc (js) failed:\n${js.stdout}\n${js.stderr}`);

  // The js pass also stubs the type-only generated barrel (`export {}`); declarations recreate it.
  rmSync(join(outDir, 'types'), { recursive: true, force: true });

  // Declarations for every subpath, the type-only entries included.
  const dts = spawnSync(process.execPath, [tsc, ...base, '--declaration', '--emitDeclarationOnly', '--outDir', outDir,
    ...entries.map(entry => join(pkgDir, entry.source))], { timeout: 120000, encoding: 'utf8' });
  if (dts.status !== 0) throw new Error(`tsc (d.ts) failed:\n${dts.stdout}\n${dts.stderr}`);

  for (const file of readdirSync(outDir, { recursive: true }).map(String).filter(name => name.endsWith('.d.ts'))) {
    const full = resolve(outDir, file);
    const text = readFileSync(full, 'utf8');
    const rewritten = rewriteDeclarationSpecifiers(text);
    if (rewritten !== text) writeFileSync(full, rewritten);
  }
}

const isMain = process.argv[1] && process.argv[1] === fileURLToPath(import.meta.url);
if (isMain) {
  try {
    buildApi();
  } catch (error) {
    console.error(error.message);
    process.exitCode = 1;
  }
}
