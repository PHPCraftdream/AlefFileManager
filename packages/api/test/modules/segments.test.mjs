// SPDX-License-Identifier: MIT OR Apache-2.0
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { readFileSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';
import { installRuntime } from '../fake-runtime.mjs';

const pkgDir = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const manifest = JSON.parse(readFileSync(resolve(pkgDir, 'package.json'), 'utf8'));
const exportsMap = manifest.exports;
const rel = p => p.replaceAll('\\', '/').slice(pkgDir.replaceAll('\\', '/').length + 1);

// Static import specifiers: `... from 'x'` and side-effect `import 'x'`.
const FROM = /(?:^|\n)\s*(?:import|export)\s[^'"\n]*?from\s*['"]([^'"]+)['"]/g;
const SIDE = /(?:^|\n)\s*import\s*['"]([^'"]+)['"]/g;
const FAMILIES = ['src/data', 'src/net', 'src/system', 'src/core'];

function isAllowed(from, to) {
  if (to.startsWith(pkgDir.replaceAll('\\', '/') + '/types/')) return true;
  if (to === pkgDir.replaceAll('\\', '/') + '/src/desktop/window.ts'
    && from === pkgDir.replaceAll('\\', '/') + '/src/desktop/menu.ts') return true;
  const fromDir = dirname(from).replaceAll('\\', '/');
  if (fromDir === dirname(to).replaceAll('\\', '/') && FAMILIES.some(f => fromDir.endsWith(f))) return true;
  if (from === pkgDir.replaceAll('\\', '/') + '/src/index.ts'
    || from === pkgDir.replaceAll('\\', '/') + '/src/core.ts') return true;
  if (to.startsWith(pkgDir.replaceAll('\\', '/') + '/src/core/')) return true;
  return false;
}

function checkEntry(entryFile) {
  const violations = [];
  const visited = new Set();
  const worklist = [entryFile];
  while (worklist.length > 0) {
    const sourceFile = worklist.pop().replaceAll('\\', '/');
    if (visited.has(sourceFile)) continue;
    visited.add(sourceFile);
    if (sourceFile.startsWith(pkgDir.replaceAll('\\', '/') + '/types/')) continue;
    const text = readFileSync(sourceFile, 'utf8');
    const specifiers = [...text.matchAll(FROM), ...text.matchAll(SIDE)].map(match => match[1]);
    for (const specifier of specifiers) {
      if (!specifier.startsWith('.')) {
        violations.push(`"${rel(sourceFile)}" must not import "${specifier}"`);
        continue;
      }
      let target = fileURLToPath(new URL(specifier, pathToFileURL(sourceFile))).replaceAll('\\', '/');
      if (!target.endsWith('.ts')) target += '.ts';
      if (!isAllowed(sourceFile, target)) {
        violations.push(`"${rel(sourceFile)}" must not import "${rel(target)}"`);
      }
      worklist.push(target);
    }
  }
  return violations;
}

test('every subpath export of @alef-runtime/api points at an existing source file', () => {
  const keys = Object.keys(exportsMap);
  assert.ok(keys.length >= 25, `expected at least 25 subpath exports, got ${keys.length}`);
  for (const [subpath, target] of Object.entries(exportsMap)) {
    const file = resolve(pkgDir, target);
    assert.ok(readFileSync(file), `export "${subpath}" points at missing file ${target}`);
  }
});

test('the import graph of every subpath stays inside core, its own module and the listed exceptions', () => {
  const all = [];
  for (const target of Object.values(exportsMap)) {
    all.push(...checkEntry(resolve(pkgDir, target)));
  }
  assert.deepEqual(all, [], 'import graph violations:\n' + all.join('\n'));
});

test('importing any subpath entry issues no runtime call', { timeout: 20000 }, async () => {
  for (const [subpath, target] of Object.entries(exportsMap)) {
    const runtime = installRuntime();
    const before = runtime.requests.length;
    await import(pathToFileURL(resolve(pkgDir, target)).href);
    const leaked = runtime.requests.length - before;
    assert.equal(runtime.requests.length, before, `subpath "${subpath}" leaked ${leaked} runtime call(s) at import time`);
  }
});
