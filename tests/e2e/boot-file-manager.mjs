// SPDX-License-Identifier: MIT OR Apache-2.0
// Boot check of the real File Manager: starts the binary with the built frontend (frontend/dist) and
// ALEF_LOG_CALLS=1, then requires the startup traffic of the new API to succeed: handshake, app.hello,
// preferences.get, the window snapshot and the event stream, with no failed request and no page error.
//   node tests/e2e/boot-file-manager.mjs --exe <alef-file-manager binary> [--frontend-dir <dir>] [--timeout-s 60] [--verbose]
import { spawn } from 'node:child_process';
import { mkdirSync, rmSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { assertQuiet, quiet } from './lib.mjs';

const here = dirname(fileURLToPath(import.meta.url));
const root = resolve(here, '..', '..');
const args = process.argv.slice(2);
const option = name => { const at = args.indexOf(name); return at < 0 ? undefined : args[at + 1]; };
const verbose = args.includes('--verbose');
const timeoutMs = Number(option('--timeout-s') ?? 60) * 1000;
const exe = option('--exe') ?? join(root, 'backend', 'target', 'debug', process.platform === 'win32' ? 'alef-file-manager.exe' : 'alef-file-manager');
const frontend = resolve(option('--frontend-dir') ?? join(root, 'frontend', 'dist'));
const scratch = join(root, 'backend', 'target', `boot-data-${process.pid}`);

// Routes the File Manager UI must have used successfully (status 2xx) by the time it is up.
const REQUIRED = [
  /^call\/runtime\.hello$/, /^call\/app\.hello$/, /^call\/preferences\.get$/,
  /^call\/runtime\.events\.subscribe$/, /^stream\/\d+$/, /^call\/window\.apply$/,
];
const SETTLE_MS = 3000;

assertQuiet(exe);
mkdirSync(scratch, { recursive: true });
const child = spawn(exe, ['--frontend-dir', frontend, '--data-dir', scratch, '--root', root], {
  env: { ...process.env, ALEF_LOG_CALLS: '1', ...(quiet ? { ALEF_E2E: '1', ALEF_E2E_QUIET: '1' } : {}) },
  stdio: ['ignore', 'ignore', 'pipe'],
});

const served = [];
const problems = [];
let settled;
let finished = false;
const finish = code => {
  if (finished) return;
  finished = true;
  clearTimeout(timer);
  clearTimeout(settled);
  child.kill();
  rmSync(scratch, { recursive: true, force: true });
  process.exitCode = code;
};

const missing = () => REQUIRED.filter(pattern => !served.some(route => pattern.test(route)));
function verdict() {
  const absent = missing();
  if (absent.length > 0) problems.push(`never seen: ${absent.join(', ')}`);
  for (const problem of problems) console.log(`File Manager boot: PROBLEM ${problem}`);
  console.log(`File Manager boot: ${served.length} requests, ${problems.length === 0 ? 'OK' : 'NOT OK'}`);
  finish(problems.length === 0 ? 0 : 1);
}

const timer = setTimeout(() => { problems.push('timed out before the startup traffic completed'); verdict(); }, timeoutMs);

let pending = '';
child.stderr.on('data', chunk => {
  pending += chunk;
  const complete = pending.split('\n');
  pending = complete.pop();
  for (const raw of complete) {
    const line = raw.trim();
    const call = /^ALEF_CALL (\S+) (\d+)(?: origin=\S*)?$/.exec(line);
    if (call) {
      console.log(line);
      const status = Number(call[2]);
      if (status < 200 || status >= 300) problems.push(`${call[1]} answered ${status}`);
      else served.push(call[1]);
    } else if (/^Servo Error:/.test(line)) {
      console.log(line);
      problems.push(line);
    } else if (verbose && line) {
      console.log(line);
    }
    // Everything required was seen: give late failures a moment to show up, then judge.
    if (missing().length === 0 && !settled) settled = setTimeout(verdict, SETTLE_MS);
  }
});
child.on('exit', () => { if (!finished) { problems.push('the runtime exited early'); verdict(); } });
