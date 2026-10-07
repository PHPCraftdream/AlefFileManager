// SPDX-License-Identifier: MIT OR Apache-2.0
// Boot check of the real File Manager: starts the generic binary `alef` on the application (the built
// frontend, frontend/dist, holds its manifest and icon) with ALEF_LOG_CALLS=1, then requires the startup
// traffic of the API to succeed: handshake, app.info, store.get, the window snapshot and the event stream,
// with no failed request and no page error. The application runs from a copy under an id of its own, so
// the data it makes is removed with the copy.
//   node tests/e2e/boot-file-manager.mjs --exe <alef binary> [--frontend-dir <dir>] [--timeout-s 60] [--verbose]
import { spawn } from 'node:child_process';
import { cpSync, existsSync, mkdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { appDataOf, assertQuiet, quiet } from './lib.mjs';

const here = dirname(fileURLToPath(import.meta.url));
const root = resolve(here, '..', '..');
const args = process.argv.slice(2);
const option = name => { const at = args.indexOf(name); return at < 0 ? undefined : args[at + 1]; };
const verbose = args.includes('--verbose');
const timeoutMs = Number(option('--timeout-s') ?? 60) * 1000;
const exe = option('--exe') ?? join(root, 'backend', 'target', 'debug', process.platform === 'win32' ? 'alef.exe' : 'alef');
const frontend = resolve(option('--frontend-dir') ?? join(root, 'frontend', 'dist'));
const scratch = join(root, 'backend', 'target', `boot-data-${process.pid}`);
const id = `org.alef.filemanager.boot${process.pid}`;
const appData = appDataOf(id);

// Routes the File Manager UI must have used successfully (status 2xx) by the time it is up.
const REQUIRED = [
  /^call\/runtime\.hello$/, /^call\/app\.info$/, /^call\/store\.get$/,
  /^call\/runtime\.events\.subscribe$/, /^stream\/\d+$/, /^call\/window\.apply$/,
];
const SETTLE_MS = 3000;

assertQuiet(exe);
if (!existsSync(join(frontend, 'alef.ktav'))) throw new Error(`${frontend} has no alef.ktav: build the frontend first (npm run build:frontend)`);
mkdirSync(scratch, { recursive: true });
const appDir = join(scratch, 'app');
cpSync(frontend, appDir, { recursive: true });
const manifest = join(appDir, 'alef.ktav');
const text = readFileSync(manifest, 'utf8');
if (!text.includes('id: org.alef.filemanager\n')) throw new Error('the manifest of the File Manager has another id');
writeFileSync(manifest, text.replace('id: org.alef.filemanager\n', `id: ${id}\n`));
const child = spawn(exe, ['--app', appDir], {
  env: {
    ...process.env, ALEF_LOG_CALLS: '1', ALEF_E2E: '1', ALEF_HOME: join(scratch, 'home'), ALEF_E2E_CONSENT: '*=allow',
    ...(quiet ? { ALEF_E2E_QUIET: '1' } : {}),
  },
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
  if (appData.endsWith(id)) rmSync(appData, { recursive: true, force: true });
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
