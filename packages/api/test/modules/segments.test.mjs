// SPDX-License-Identifier: MIT OR Apache-2.0
import assert from 'node:assert/strict';
import { test } from 'node:test';
import { spawnSync } from 'node:child_process';
import { readFileSync, readdirSync } from 'node:fs';
import { dirname, resolve } from 'node:path';
import { fileURLToPath, pathToFileURL } from 'node:url';

const pkgDir = resolve(dirname(fileURLToPath(import.meta.url)), '../..');
const pkgUrl = pkgDir.replaceAll('\\', '/');
const manifest = JSON.parse(readFileSync(resolve(pkgDir, 'package.json'), 'utf8'));
const exportsMap = manifest.exports;
const rel = p => p.replaceAll('\\', '/').slice(pkgUrl.length + 1);
const under = (file, dir) => file.startsWith(`${pkgUrl}/${dir}/`);

// Static import specifiers: `... from 'x'` and side-effect `import 'x'`; both span lines.
const FROM = /(?:^|\n)\s*(?:import|export)\s[\s\S]*?from\s*['"]([^'"]+)['"]/g;
const SIDE = /(?:^|\n)\s*import\s*['"]([^'"]+)['"]/g;

// Explicit edges between modules; everything else outside src/core/ is a violation.
const EDGES = [
  // client and server share one transport and body handling
  { from: 'src/net/http.ts', to: 'src/net/http-server.ts' },
  { from: 'src/net/http.ts', to: 'src/net/body.ts' },
  // the server reads bodies and upgrades to WebSocket connections
  { from: 'src/net/http-server.ts', to: 'src/net/body.ts' },
  { from: 'src/net/http-server.ts', to: 'src/net/websocket-connection.ts' },
  // the WebSocket client and server build on the HTTP transport
  { from: 'src/net/websocket.ts', to: 'src/net/http.ts' },
  { from: 'src/net/websocket.ts', to: 'src/net/http-server.ts' },
  { from: 'src/net/websocket.ts', to: 'src/net/websocket-connection.ts' },
  // a menu attaches to a window
  { from: 'src/desktop/menu.ts', to: 'src/desktop/window.ts', typeOnly: true },
];


function isAllowed(from, to) {
  if (under(to, 'types')) return true;
  if (under(to, 'src/core')) return true;
  if (under(from, 'src/core') && dirname(from) === dirname(to)) return true;
  if (from === `${pkgUrl}/src/index.ts` || from === `${pkgUrl}/src/core.ts`) return true;
  return EDGES.some(edge => from === `${pkgUrl}/${edge.from}` && to === `${pkgUrl}/${edge.to}`);
}

function typeOnlyViolations(sourceFile, text) {
  const edge = EDGES.find(e => sourceFile === `${pkgUrl}/${e.from}` && e.typeOnly);
  if (!edge) return [];
  const target = resolve(pkgUrl, edge.to);
  const statements = [...text.matchAll(FROM)].filter(match => resolve(dirname(sourceFile), match[1]) === target);
  if (statements.length === 0) return [`"${rel(sourceFile)}" must keep the type-only import of "${edge.to}"`];
  return statements
    .filter(match => !/^\s*import\s+type\s/.test(match[0]))
    .map(match => `"${rel(sourceFile)}" must import "${edge.to}" only with "import type": ${match[0].trim()}`);
}

function checkEntry(entryFile) {
  const violations = [];
  const visited = new Set();
  const worklist = [entryFile];
  while (worklist.length > 0) {
    const sourceFile = worklist.pop().replaceAll('\\', '/');
    if (visited.has(sourceFile)) continue;
    visited.add(sourceFile);
    if (under(sourceFile, 'types')) continue; // the leaf of types is never walked
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
    violations.push(...typeOnlyViolations(sourceFile, text));
  }
  return violations;
}

// A runtime entry must not be imported through `./types`: the test checks that directory statically.
const runtimeEntries = Object.entries(exportsMap).filter(([, target]) => !under(resolve(pkgDir, target).replaceAll('\\', '/'), 'types'));

test('every subpath export of @alef-runtime/api points at an existing source file', () => {
  const keys = Object.keys(exportsMap);
  assert.ok(keys.length >= 25, `expected at least 25 subpath exports, got ${keys.length}`);
  for (const [subpath, target] of Object.entries(exportsMap)) {
    const file = resolve(pkgDir, target);
    assert.ok(readFileSync(file), `export "${subpath}" points at missing file ${target}`);
  }
});

test('the generated types directory declares and re-exports only types', () => {
  const dir = resolve(pkgDir, 'types');
  const allowed = /^(export\s+type\b|import\s+type\b|export\s+\*\s+from\b)/;
  const files = readdirSync(dir, { recursive: true }).map(String).filter(name => name.endsWith('.ts'));
  assert.ok(files.length > 0);
  for (const name of files) {
    const text = readFileSync(resolve(dir, name), 'utf8').replace(/\/\*[\s\S]*?\*\//g, '').replace(/\/\/[^\n]*/g, '');
    let depth = 0;
    let open = false;
    for (const raw of text.split('\n')) {
      const line = raw.trim();
      if (!line) continue;
      if (depth === 0 && !open) assert.match(line, allowed, `"types/${name}" has a statement that is not a type: ${line}`);
      for (const ch of line) {
        if ('{(['.includes(ch)) depth++;
        else if ('})]'.includes(ch)) depth--;
      }
      open = !(depth === 0 && line.endsWith(';'));
    }
  }
});

test('a value import beside the type-only edge is caught', () => {
  const menu = `${pkgUrl}/src/desktop/menu.ts`;
  assert.deepEqual(typeOnlyViolations(menu, "import type { AppWindow } from './window.ts';\n"), []);
  assert.equal(typeOnlyViolations(menu, "import type { AppWindow } from './window.ts';\nimport { nativeWindow } from './window.ts';\n").length, 1);
});

test('the import parser reads import and export statements that span several lines', () => {
  const sample = 'import {\n  fs,\n} from \'./desktop/window.ts\';\n';
  assert.equal([...sample.matchAll(FROM)][0][1], './desktop/window.ts');
  const specifiers = [...readFileSync(resolve(pkgDir, 'src/index.ts'), 'utf8').matchAll(FROM)].map(match => match[1]);
  assert.ok(specifiers.includes('./desktop/window.ts'), 'the multiline exports of src/index.ts must be parsed');
});

test('the import graph of every subpath stays inside core, its own module and the listed edges', () => {
  const all = [];
  for (const [, target] of runtimeEntries) {
    all.push(...checkEntry(resolve(pkgDir, target)));
  }
  assert.deepEqual(all, [], 'import graph violations:\n' + all.join('\n'));
});

test('importing any subpath entry issues no runtime call', { timeout: 60000 }, () => {
  // The child sees its entry and helper as file:// URLs through the environment, not argv.
  const childScript = `
    const { installRuntime } = await import(process.env.ALEF_HELPER);
    const runtime = installRuntime();
    let listeners = 0;
    const wrap = target => {
      const real = target.addEventListener?.bind(target);
      target.addEventListener = (...args) => {
        listeners += 1;
        return real?.(...args);
      };
    };
    wrap(globalThis);
    for (const key of ['window', 'document']) {
      if (!globalThis[key]) globalThis[key] = {};
      wrap(globalThis[key]);
    }
    let timers = 0;
    const realTimeout = globalThis.setTimeout.bind(globalThis);
    const realInterval = globalThis.setInterval.bind(globalThis);
    globalThis.setTimeout = (...args) => {
      timers += 1;
      return realTimeout(...args);
    };
    globalThis.setInterval = (...args) => {
      timers += 1;
      return realInterval(...args);
    };
    await import(process.env.ALEF_ENTRY);
    console.log(JSON.stringify({ requests: runtime.requests.length, listeners, timers }));
  `;
  for (const [subpath, target] of runtimeEntries) {
    const run = spawnSync(process.execPath, ['--experimental-strip-types', '--input-type=module', '-e', childScript], {
      timeout: 20000,
      encoding: 'utf8',
      env: {
        ...process.env,
        ALEF_ENTRY: pathToFileURL(resolve(pkgDir, target)).href,
        ALEF_HELPER: pathToFileURL(resolve(pkgDir, 'test/fake-runtime.mjs')).href,
      },
    });
    assert.equal(run.status, 0, `subpath "${subpath}" failed to import: ${run.stderr}`);
    const counts = JSON.parse(run.stdout.trim());
    assert.equal(counts.requests, 0, `subpath "${subpath}" issued ${counts.requests} runtime call(s) at import time`);
    assert.equal(counts.listeners, 0, `subpath "${subpath}" registered ${counts.listeners} listener(s) at import time`);
    assert.equal(counts.timers, 0, `subpath "${subpath}" scheduled ${counts.timers} timer(s) at import time`);
  }
});
