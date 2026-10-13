// SPDX-License-Identifier: MIT OR Apache-2.0
// CI planner: computes what a push/pull_request/manual run has to execute; the workflow consumes the JSON outputs.
//   node scripts/ci/plan.mjs --event push --ref refs/heads/feature --before <sha> --after <sha> [--message "..."] [--full]
// Unknown base (git diff fails or yields nothing) => full plan. Pure logic lives in computePlan; tests inject everything.

import { readdirSync, readFileSync, existsSync, appendFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { fileURLToPath } from 'node:url';
import path from 'node:path';

// Convert a glob to an anchored regex: '**' -> '.*', '*' -> '[^/]*', '?' -> '[^/]', rest escaped.
export function matchGlob(path, glob) {
  let source = '';
  for (let i = 0; i < glob.length; i++) {
    if (glob.startsWith('**', i)) {
      source += '.*';
      i++;
    } else if (glob[i] === '*') {
      source += '[^/]*';
    } else if (glob[i] === '?') {
      source += '[^/]';
    } else {
      source += glob[i].replace(/[.+^${}()|[\]\\]/g, '\\$&');
    }
  }
  return new RegExp(`^${source}$`).test(path);
}

export function matchesAny(path, globs) {
  return globs.some(glob => matchGlob(path, glob));
}

// Docs-only: every file is docs/** or *.md; empty means NOT docs-only.
export function isDocsOnly(files) {
  if (files.length === 0) return false;
  return files.every(f => matchGlob(f, 'docs/**') || f.endsWith('.md'));
}

// Sorted unique set of changed crates plus all crates depending on them, transitively.
export function reverseDependents(changed, graph) {
  const set = new Set();
  const queue = [...changed];
  while (queue.length > 0) {
    const crate = queue.pop();
    if (set.has(crate)) continue;
    set.add(crate);
    for (const [name, deps] of Object.entries(graph)) {
      if (deps.includes(crate) && !set.has(name)) queue.push(name);
    }
  }
  return [...set].sort();
}

// Names of crates under backend/crates/<name>/ in files; workspace manifest/lock => all.
export function affectedCrates(files, allCrates, graph) {
  if (files.includes('backend/Cargo.lock') || files.includes('backend/Cargo.toml')) return [...allCrates];
  const hits = [];
  for (const file of files) {
    const m = file.match(/^backend\/crates\/([A-Za-z0-9_-]+)\//);
    if (m && allCrates.includes(m[1])) hits.push(m[1]);
  }
  return reverseDependents(hits, graph);
}

// JS packages affected by changed files; full => ["all"]. Returns sorted folder list.
export function affectedJs(files, packages, packageDeps, full) {
  if (full) return ['all'];
  if (files.includes('package.json') || files.includes('package-lock.json')) return ['all'];
  const direct = new Set();
  for (const file of files) {
    for (const pkg of packages) {
      if (file.startsWith(`${pkg.folder}/`)) direct.add(pkg.folder);
    }
  }
  // transitive dependents: package whose dependencies reference a changed package name or folder basename
  const queue = [...direct];
  while (queue.length > 0) {
    const folder = queue.pop();
    const basename = folder.split('/')[1];
    const name = packages.find(p => p.folder === folder)?.name;
    for (const pkg of packages) {
      if (pkg.folder === folder || direct.has(pkg.folder)) continue;
      const deps = pkg.dependencies ?? {};
      const depends = Object.keys(deps).some(k => k === name || k === basename);
      if (depends && !direct.has(pkg.folder)) {
        direct.add(pkg.folder);
        queue.push(pkg.folder);
      }
    }
  }
  const result = new Set(direct);
  if (files.some(f => matchGlob(f, 'frontend/**'))) result.add('frontend');
  if (files.some(f => matchGlob(f, 'scripts/**'))) result.add('scripts');
  return [...result].sort();
}

// e2e scenarios for changed files: 'all' | 'none' | comma-joined sorted unique list.
export function e2eFor(files, rules, smoke, full, docsOnly) {
  if (full) return 'all';
  if (docsOnly) return 'none';
  const scenarios = new Set();
  for (const file of files) {
    for (const rule of rules) {
      if (matchGlob(file, rule.glob)) for (const s of rule.scenarios) scenarios.add(s);
    }
  }
  // harness-level or core-layer changes pull in the whole smoke set
  const smokeHit = files.some(f =>
    ['tests/e2e/harness.js', 'tests/e2e/lib.mjs', 'tests/e2e/run.mjs', 'tests/e2e/ws-server.mjs'].includes(f) ||
    matchGlob(f, 'tests/e2e/apps/**') ||
    matchGlob(f, 'tests/e2e/scenarios/**') ||
    matchGlob(f, 'backend/crates/alef-core/**') ||
    matchGlob(f, 'backend/crates/alef-runtime/src/bridge/**'));
  if (smokeHit) for (const s of smoke) scenarios.add(s);
  // scenario files contribute their own scenario, derived from the file name
  for (const file of files) {
    const m = file.match(/^tests\/e2e\/scenarios\/[^/]+\.mjs$/);
    if (m) scenarios.add(path.basename(file).replace(/\.mjs$/, ''));
  }
  if (scenarios.size === 0) return 'none';
  return [...scenarios].sort().join(',');
}

const KNOWN_PATHS = [
  'docs/**', '*.md', 'backend/**', 'packages/**', 'frontend/**', 'scripts/**', 'tests/**',
  'package.json', 'package-lock.json', 'rust-toolchain*', '.oxlintrc.json',
];

function lastSegment(ref) {
  return ref.split('/').pop();
}

export function computePlan(input) {
  const {
    event, ref, files, filesKnown, message, manualFull,
    runnersAll, runnersLinux, fullTriggers, platformSensitive,
    e2eRules, e2eSmoke, allCrates, crateGraph, packages,
  } = input;

  const docsOnly = filesKnown && isDocsOnly(files);
  const triggerFile = fullTriggers.find(g => files.some(f => matchGlob(f, g)));
  const sensitiveFile = platformSensitive.find(g => files.some(f => matchGlob(f, g)));
  const unknownFile = filesKnown && files.find(f => !matchesAny(f, KNOWN_PATHS));

  // Full plan triggers, first match wins for the reason.
  let full = false;
  let reason = '';
  if (event === 'schedule') { full = true; reason = 'nightly schedule runs the full matrix'; }
  else if (event === 'push' && lastSegment(ref) === 'main') { full = true; reason = 'push to main runs the full matrix'; }
  else if (manualFull) { full = true; reason = 'manual full=true'; }
  else if (message.includes('[full]')) { full = true; reason = '[full] in the commit message'; }
  else if (!filesKnown) { full = true; reason = 'changed files unknown'; }
  else if (triggerFile) { full = true; reason = `changed ${triggerFile}`; }
  else if (unknownFile) { full = true; reason = `path no rule knows: ${unknownFile}`; }

  const plan = {
    full, os: runnersAll, crates: 'all', js: ['all'], e2e: 'all',
    lint: true, structure: true, fmt: true, reason,
    crates_all: true, js_all: true, docs_only: false,
  };
  if (full) return plan;

  if (docsOnly) {
    plan.docs_only = true;
    plan.os = ['ubuntu-24.04'];
    plan.crates = [];
    plan.crates_all = false;
    plan.js = [];
    plan.js_all = false;
    plan.e2e = 'none';
    plan.reason = 'docs-only change: structure+fmt only';
    return plan;
  }

  const crates = affectedCrates(files, allCrates, crateGraph);
  plan.crates_all = crates.length === 0;
  plan.crates = crates.length === 0 ? 'all' : crates;

  const js = affectedJs(files, packages, null, false);
  plan.js_all = false;
  plan.js = js;

  const e2e = e2eFor(files, e2eRules, e2eSmoke, false, false);
  plan.e2e = e2e;

  if (sensitiveFile) {
    plan.os = runnersAll;
    plan.reason = `platform-sensitive: ${sensitiveFile}; e2e ${e2e}`;
  } else if (e2e !== 'none') {
    plan.os = runnersAll;
    plan.reason = `e2e scenarios: ${e2e}; full os matrix`;
  } else {
    plan.os = runnersLinux;
    plan.reason = 'affected-only run; linux only';
  }
  return plan;
}

// Fallback: a CI run must not die because the planner hiccuped.
function fullFallback(reason, data) {
  return {
    full: true, os: data.runners_all, crates: 'all', js: ['all'], e2e: 'all',
    lint: true, structure: true, fmt: true, reason, crates_all: true, js_all: true, docs_only: false,
  };
}

function parseArgs(argv) {
  const args = {};
  for (let i = 0; i < argv.length; i++) {
    const flag = argv[i];
    if (!flag.startsWith('--')) continue;
    const next = argv[i + 1];
    if (flag === '--full' && (next === 'true' || next === 'false')) {
      // workflow passes the dispatch input's value; bare --full stays true
      args[flag] = next;
      i++;
    } else if (next === undefined || next.startsWith('--')) {
      args[flag] = true;
    } else {
      args[flag] = next;
      i++;
    }
  }
  return args;
}

// Crate dependency graph: cargo metadata first; Cargo.toml path-dep regex as fallback.
function buildCrateGraph(root) {
  const cratesDir = path.join(root, 'backend', 'crates');
  if (!existsSync(cratesDir)) return { allCrates: [], crateGraph: {} };
  const allCrates = readdirSync(cratesDir, { withFileTypes: true })
    .filter(e => e.isDirectory()).map(e => e.name);
  const memberSet = new Set(allCrates);
  let crateGraph = null;
  const meta = spawnSync('cargo', ['metadata', '--no-deps', '--format-version', '1'], {
    cwd: path.join(root, 'backend'), encoding: 'utf8', timeout: 30000,
  });
  if (meta.status === 0 && meta.stdout) {
    try {
      const parsed = JSON.parse(meta.stdout);
      crateGraph = {};
      for (const pkg of parsed.packages ?? []) {
        if (!memberSet.has(pkg.name)) continue;
        crateGraph[pkg.name] = (pkg.dependencies ?? [])
          .map(d => d.name ?? d)
          .filter(n => memberSet.has(n));
      }
    } catch { /* fall through to Cargo.toml parsing */ }
  }
  if (!crateGraph) {
    // If both sources fail, an empty graph keeps reverseDependents conservative: just the changed crates.
    crateGraph = {};
    for (const crate of allCrates) {
      let toml = '';
      try { toml = readFileSync(path.join(cratesDir, crate, 'Cargo.toml'), 'utf8'); } catch { continue; }
      const deps = [];
      for (const m of toml.matchAll(/([A-Za-z0-9_-]+)\s*=\s*\{\s*path\s*=\s*"\.\.\/([A-Za-z0-9_-]+)"/g)) {
        if (memberSet.has(m[2])) deps.push(m[2]);
      }
      crateGraph[crate] = deps;
    }
  }
  return { allCrates, crateGraph };
}

function buildPackages(root) {
  const packagesDir = path.join(root, 'packages');
  if (!existsSync(packagesDir)) return [];
  const packages = [];
  for (const entry of readdirSync(packagesDir, { withFileTypes: true })) {
    if (!entry.isDirectory()) continue;
    const manifest = path.join(packagesDir, entry.name, 'package.json');
    if (!existsSync(manifest)) continue;
    try {
      const pkg = JSON.parse(readFileSync(manifest, 'utf8'));
      packages.push({ folder: `packages/${entry.name}`, name: pkg.name, dependencies: pkg.dependencies ?? {} });
    } catch { /* skip unreadable manifests */ }
  }
  return packages;
}

function main() {
  const root = fileURLToPath(new URL('../../', import.meta.url));
  const data = JSON.parse(readFileSync(path.join(root, 'scripts', 'ci', 'affected.json'), 'utf8'));
  const args = parseArgs(process.argv.slice(2));

  let event = args['--event'] ?? process.env.GITHUB_EVENT_NAME ?? '';
  const ref = args['--ref'] ?? process.env.GITHUB_REF ?? '';
  const message = args['--message'] ?? process.env.COMMIT_MESSAGE ?? '';
  const manualFull = (args['--full'] === true || args['--full'] === 'true') || process.env.INPUT_FULL === 'true';
  const before = args['--before'];
  const after = args['--after'];

  const rawFiles = args['--files'];
  let files;
  let filesKnown;
  if (rawFiles !== undefined) {
    // empty --files string means empty-by-error => unknown
    files = String(rawFiles).split(/[\n,]/).map(s => s.trim()).filter(Boolean);
    filesKnown = files.length > 0;
  } else if (before && after) {
    const diff = spawnSync('git', ['diff', '--name-only', before, after], {
      cwd: root, encoding: 'utf8', timeout: 30000,
    });
    if (diff.status === 0 && diff.stdout.trim().length > 0) {
      files = diff.stdout.split('\n').map(s => s.trim()).filter(Boolean);
      filesKnown = true;
    } else {
      files = [];
      filesKnown = false;
    }
  } else {
    files = [];
    filesKnown = false;
  }

  const { allCrates, crateGraph } = buildCrateGraph(root);
  const packages = buildPackages(root);

  let plan;
  try {
    plan = computePlan({
      event, ref, files, filesKnown, message, manualFull,
      runnersAll: data.runners_all, runnersLinux: data.runners_linux,
      fullTriggers: data.full_triggers, platformSensitive: data.platform_sensitive,
      e2eRules: data.e2e_rules, e2eSmoke: data.e2e_smoke,
      allCrates, crateGraph, packages,
    });
  } catch (error) {
    plan = fullFallback(`planner error: ${error?.message ?? String(error)}`, data);
  }
  console.log(JSON.stringify(plan, null, 2));

  if (process.env.GITHUB_OUTPUT) {
    const lines = [
      `full=${plan.full}`,
      `os=${JSON.stringify(plan.os)}`,
      `crates=${Array.isArray(plan.crates) ? plan.crates.join(',') : plan.crates}`,
      `js=${Array.isArray(plan.js) ? plan.js.join(',') : plan.js}`,
      `e2e=${plan.e2e}`,
      `docs_only=${plan.docs_only}`,
    ];
    appendFileSync(process.env.GITHUB_OUTPUT, lines.join('\n') + '\n');
  }
}

const isMain = process.argv[1] && process.argv[1] === fileURLToPath(import.meta.url);
if (isMain) {
  try {
    main();
  } catch (error) {
    // Never throw on bad input: emit a full fallback plan and exit 0.
    const root = fileURLToPath(new URL('../../', import.meta.url));
    let data = { runners_all: ['ubuntu-24.04'] };
    try { data = JSON.parse(readFileSync(path.join(root, 'scripts', 'ci', 'affected.json'), 'utf8')); } catch { /* defaults */ }
    console.log(JSON.stringify(fullFallback(`planner error: ${error?.message ?? String(error)}`, data), null, 2));
  }
}
