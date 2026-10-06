// SPDX-License-Identifier: MIT OR Apache-2.0
// M1 transport smoke runner: transpiles @alef-tron/api next to the smoke page, starts the File Manager
// binary with it and waits for the verdict the page reports (`M1_SMOKE RESULT PASS|FAIL`).
// Exit code 0 = PASS.
//   node experiments/m1-smoke/run.mjs --exe <binary> [--expect-fail] [--timeout-s 150] [--verbose]
// `--expect-fail` runs with the induced failure (ALEF_M1_SMOKE_BREAK=1: the binary echo is corrupted)
// and succeeds only if exactly the two binary-echo checks are reported as failed.
import { spawn, spawnSync } from 'node:child_process';
import { copyFileSync, mkdirSync, rmSync } from 'node:fs';
import { createRequire } from 'node:module';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const root = resolve(here, '..', '..');
const args = process.argv.slice(2);
const option = name => { const at = args.indexOf(name); return at < 0 ? undefined : args[at + 1]; };
const expectFail = args.includes('--expect-fail');
const verbose = args.includes('--verbose');
const timeoutMs = Number(option('--timeout-s') ?? 150) * 1000;
const exe = option('--exe') ?? join(root, 'backend', 'target', 'debug', process.platform === 'win32' ? 'alef-file-manager.exe' : 'alef-file-manager');
const site = join(root, 'backend', 'target', `m1-smoke-site-${process.pid}`);
const scratch = join(root, 'backend', 'target', `m1-smoke-data-${process.pid}`);
const maxAbortMs = 250;
const windowBytes = 1024 * 1024;

// Every check the page must report as ok in a PASS run (the page cannot silently skip one).
const EXPECTED = [
  'hello', 'denials', 'json-echo', 'binary-echo-16MiB', 'app-command-on-registry', 'legacy-invoke-route-is-gone',
  'credit-window', 'abort-closes-the-source', 'events-stream',
  'lib-connect', 'lib-call-json', 'lib-binary-roundtrip-4MiB', 'lib-error-mapping', 'lib-readable-acks-by-itself',
  'lib-close-stops-the-source', 'lib-events', 'lib-window-watch',
  ...['navigation', 'reload'].flatMap(how => [`${how}-new-token`, `${how}-old-token-denied`, `${how}-library-reconnects`]),
];
const INDUCED = ['binary-echo-16MiB', 'lib-binary-roundtrip-4MiB'];

function buildSite() {
  mkdirSync(site, { recursive: true });
  for (const name of ['index.html', 'smoke.js']) copyFileSync(join(here, name), join(site, name));
  const tsc = join(dirname(createRequire(import.meta.url).resolve('typescript/package.json')), 'bin', 'tsc');
  const api = join(root, 'packages', 'api');
  const result = spawnSync(process.execPath, [
    tsc, '--ignoreConfig', '--strict', '--skipLibCheck', '--target', 'ES2022', '--module', 'ESNext',
    '--moduleResolution', 'bundler', '--rewriteRelativeImportExtensions',
    '--rootDir', api, '--outDir', site, join(api, 'src', 'index.ts'),
  ], { cwd: root, stdio: 'inherit' });
  if (result.status !== 0) throw new Error('could not transpile @alef-tron/api for the smoke page');
}

buildSite();
mkdirSync(scratch, { recursive: true });
const child = spawn(exe, ['--frontend-dir', site, '--data-dir', scratch, '--root', root], {
  env: { ...process.env, ALEF_M1_SMOKE: '1', ...(expectFail ? { ALEF_M1_SMOKE_BREAK: '1' } : {}) },
  stdio: ['ignore', 'ignore', 'pipe'],
});

const lines = [];
let verdict;
let finished = false;
const finish = code => {
  if (finished) return;
  finished = true;
  clearTimeout(timer);
  child.kill();
  rmSync(scratch, { recursive: true, force: true });
  rmSync(site, { recursive: true, force: true });
  process.exitCode = code;
};

const timer = setTimeout(() => { console.error('M1 smoke: timed out without a verdict'); finish(2); }, timeoutMs);
const field = (text, key) => Number(new RegExp(`${key}=(\\d+)`).exec(text)?.[1]);

// Returns the problems found in the runtime log; empty when every property holds.
function judge(result, failed) {
  const problems = [];
  if (expectFail) {
    if (result !== 'FAIL') problems.push('the induced failure was not detected');
    else if (JSON.stringify([...failed].sort()) !== JSON.stringify([...INDUCED].sort())) {
      problems.push(`expected exactly ${INDUCED} to fail, got ${failed}`);
    }
    return problems;
  }
  if (result !== 'PASS') problems.push(`verdict ${result}: ${failed}`);
  const ok = new Set(lines.map(line => /check (\S+) ok /.exec(line)?.[1]).filter(Boolean));
  const missing = EXPECTED.filter(name => !ok.has(name));
  if (missing.length > 0) problems.push(`checks without an ok line: ${missing}`);

  const stopped = new Map(lines.filter(line => line.includes('producer-stopped')).map(line => [field(line, 'stream'), field(line, 'at_ms')]));
  const aborts = lines.filter(line => line.includes('abort-called'));
  if (aborts.length < 2) problems.push(`expected an abort from the raw and from the library check, saw ${aborts.length}`);
  for (const line of aborts) {
    const stream = field(line, 'stream');
    const delta = stopped.get(stream) - field(line, 'at_ms');
    console.log(`M1 smoke: abort of stream ${stream} -> source closed in ${delta} ms (limit ${maxAbortMs} ms, e2e target 100 ms)`);
    if (!(delta >= 0 && delta <= maxAbortMs)) problems.push(`stream ${stream}: source stopped after ${delta} ms`);
  }
  const floods = lines.filter(line => line.includes('flood-done'));
  if (floods.length < 2) problems.push(`expected two completed floods, saw ${floods.length}`);
  for (const line of floods) {
    const peak = field(line, 'peak_outstanding');
    console.log(`M1 smoke: ${line.trim()}`);
    if (!(peak > 0 && peak <= windowBytes)) problems.push(`credit window exceeded: ${line.trim()}`);
  }
  return problems;
}

let pending = '';
child.stderr.on('data', chunk => {
  pending += chunk;
  const complete = pending.split('\n');
  pending = complete.pop();
  for (const line of complete) {
    if (!line.includes('M1_SMOKE')) {
      if (verbose) console.log(line.trim());
      continue;
    }
    console.log(line.trim());
    lines.push(line);
    const result = /M1_SMOKE RESULT (PASS|FAIL)\s*(.*)$/.exec(line);
    if (result && !verdict) {
      verdict = result[1];
      const problems = judge(verdict, result[2].split(',').filter(Boolean));
      for (const problem of problems) console.log(`M1 smoke: PROBLEM ${problem}`);
      const passed = problems.length === 0;
      console.log(`M1 smoke: verdict ${verdict}${expectFail ? ' (failure was induced; detecting it is the pass condition)' : ''} -> ${passed ? 'OK' : 'NOT OK'}`);
      finish(passed ? 0 : 1);
    }
  }
});
child.on('exit', () => { if (!finished) { console.error('M1 smoke: the runtime exited without a verdict'); finish(3); } });
