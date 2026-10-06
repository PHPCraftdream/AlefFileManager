// SPDX-License-Identifier: MIT OR Apache-2.0
// M1 transport smoke runner: starts the File Manager binary with the smoke page and waits for the
// verdict line the page reports (`M1_SMOKE RESULT PASS|FAIL`). Exit code 0 = PASS.
//   node experiments/m1-smoke/run.mjs --exe <binary> [--expect-fail] [--timeout-s 150] [--verbose]
// `--expect-fail` runs with the induced failure (ALEF_M1_SMOKE_BREAK=1) and succeeds only if FAIL is seen.
import { spawn } from 'node:child_process';
import { mkdirSync, rmSync } from 'node:fs';
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
const scratch = join(root, 'backend', 'target', `m1-smoke-data-${process.pid}`);
const maxAbortMs = 250;

mkdirSync(scratch, { recursive: true });
const child = spawn(exe, ['--frontend-dir', here, '--data-dir', scratch, '--root', root], {
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
  process.exitCode = code;
};

const at = (text, key) => Number(new RegExp(`${key}=(\\d+)`).exec(text)?.[1]);
const timer = setTimeout(() => { console.error('M1 smoke: timed out without a verdict'); finish(2); }, timeoutMs);

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
    const result = /M1_SMOKE RESULT (PASS|FAIL)/.exec(line);
    if (result && !verdict) {
      verdict = result[1];
      const called = lines.find(item => item.includes('abort-called'));
      const stopped = lines.find(item => item.includes('producer-stopped'));
      let abortOk = true;
      if (called && stopped) {
        const delta = at(stopped, 'at_ms') - at(called, 'at_ms');
        abortOk = delta >= 0 && delta <= maxAbortMs;
        console.log(`M1 smoke: abort -> source closed in ${delta} ms (limit ${maxAbortMs} ms, e2e target 100 ms)`);
      } else if (!expectFail) {
        console.log('M1 smoke: abort evidence missing in the runtime log');
        abortOk = false;
      }
      const flood = lines.find(item => item.includes('flood-done'));
      if (flood) console.log(`M1 smoke: ${flood.trim()}`);
      const passed = expectFail ? verdict === 'FAIL' : verdict === 'PASS' && abortOk;
      console.log(`M1 smoke: verdict ${verdict}${expectFail ? ' (failure was induced; detecting it is the pass condition)' : ''} -> ${passed ? 'OK' : 'NOT OK'}`);
      finish(passed ? 0 : 1);
    }
  }
});
child.on('exit', () => { if (!finished) { console.error('M1 smoke: the runtime exited without a verdict'); finish(3); } });
