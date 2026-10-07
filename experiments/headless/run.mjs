// M0.5 spike runner: starts `alef` in the headless mode on app/ and measures it.
//   node experiments/headless/run.mjs [--exe <alef>] [--hold-ms 8000] [--idle-ms 250] [--verbose]
// Prints one JSON object: what worked, time from the start to the first app.info, memory and CPU at rest.
import { spawn, spawnSync } from 'node:child_process';
import { cpSync, copyFileSync, existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync } from 'node:fs';
import os from 'node:os';
import { dirname, join, resolve } from 'node:path';
import { fileURLToPath } from 'node:url';

import { exeName, root, transpileApi } from '../../tests/e2e/lib.mjs';

const here = dirname(fileURLToPath(import.meta.url));
const args = process.argv.slice(2);
const option = (name, fallback) => { const at = args.indexOf(name); return at < 0 ? fallback : args[at + 1]; };
const exe = resolve(option('--exe', join(root, 'backend', 'target', 'debug', `alef${exeName}`)));
const hold = Number(option('--hold-ms', 8000));
const idle = option('--idle-ms', '250');
const verbose = args.includes('--verbose');
const raf = args.includes('--raf');
const windowed = args.includes('--windowed');
const throttle = args.includes('--throttle');

const scratch = mkdtempSync(join(os.tmpdir(), 'alef-headless-'));
const site = join(scratch, 'site');
cpSync(join(here, 'app'), site, { recursive: true });
copyFileSync(join(root, 'tests', 'e2e', 'harness.js'), join(site, 'harness.js'));
const api = transpileApi();
for (const folder of ['src', 'types']) cpSync(join(api, folder), join(site, folder), { recursive: true });

/** Memory (bytes), CPU seconds and whether the process has a window, for a process id. */
function sample(pid) {
  if (process.platform === 'win32') {
    const script = `$p = Get-Process -Id ${pid}; "$($p.WorkingSet64) $($p.TotalProcessorTime.TotalSeconds) $($p.MainWindowHandle) $($p.PrivateMemorySize64) $($p.PeakWorkingSet64)"`;
    const out = spawnSync('powershell', ['-NoProfile', '-Command', script], { encoding: 'utf8' }).stdout.trim().split(' ');
    return { rss: Number(out[0]), cpu: Number(out[1]), window: Number(out[2]) !== 0, privateBytes: Number(out[3]), peak: Number(out[4]) };
  }
  const out = spawnSync('ps', ['-o', 'rss=,cputime=', '-p', String(pid)], { encoding: 'utf8' }).stdout.trim().split(/\s+/);
  // cputime is [[hh:]mm:]ss, from the right.
  const [s = 0, m = 0, h = 0] = out[1].split(':').map(Number).reverse();
  return { rss: Number(out[0]) * 1024, cpu: h * 3600 + m * 60 + s, window: false, privateBytes: Number(out[0]) * 1024, peak: Number(out[0]) * 1024 };
}

const env = {
  ...process.env,
  ...(windowed ? { ALEF_E2E_QUIET: '1' } : { ALEF_SPIKE_HEADLESS: '1' }), ...(throttle ? { ALEF_SPIKE_THROTTLE: '1' } : {}), ALEF_SPIKE_IDLE_MS: idle, ALEF_E2E: '1', ALEF_E2E_CONSENT: '*=allow',
  ALEF_HOME: join(scratch, 'home'),
};
if (args.includes('--desktop')) {
  // Windows: a desktop nobody sits at, the closest a regular session gets to session 0.
  const log = join(scratch, 'desktop.log');
  const command = `"${exe}" --app "${site}" -- --hold-ms=${hold}`;
  const ran = spawnSync('powershell', ['-NoProfile', '-File', join(here, 'desktop.ps1'), '-Command', command, '-Out', log], { env, encoding: 'utf8', timeout: 180000 });
  const seen = (existsSync(log) ? readFileSync(log, 'utf8') : '').split('\n').map(line => line.trim()).filter(Boolean);
  console.log(JSON.stringify({
    mode: 'headless on a desktop nobody sits at', powershell: ran.stdout.trim(),
    app: seen.filter(line => /HEADLESS|panicked|Cannot|Error/.test(line)).slice(0, 30),
  }, null, 2));
  rmSync(scratch, { recursive: true, force: true });
  process.exit(0);
}
const started = Date.now();
const child = spawn(exe, ['--app', site, '--', `--hold-ms=${hold}`, ...(raf ? ['--raf'] : [])], { env, stdio: ['ignore', 'pipe', 'pipe'] });
const lines = [];
const marks = {};
let exit = null;
const collect = stream => {
  let buffer = '';
  stream.on('data', chunk => {
    buffer += chunk;
    const parts = buffer.split('\n');
    buffer = parts.pop();
    for (const raw of parts) {
      const line = raw.trim();
      if (!line) continue;
      lines.push(line);
      if (verbose) console.error(`    | ${line}`);
      if (line.includes('HEADLESS software-context ok')) marks.context ??= Date.now() - started;
      if (line.includes('HEADLESS webview ok')) marks.webview ??= Date.now() - started;
      if (line.includes('HEADLESS app.info')) marks.appInfo ??= Date.now() - started;
    }
  });
};
collect(child.stderr);
collect(child.stdout);
child.on('exit', (code, signal) => { exit = { code, signal }; });

const wait = ms => new Promise(resolvePromise => setTimeout(resolvePromise, ms));
const until = async (predicate, ms) => {
  const deadline = Date.now() + ms;
  while (!predicate() && Date.now() < deadline) await wait(50);
  return predicate();
};

const result = { exe, platform: process.platform, mode: windowed ? 'windowed (quiet)' : 'headless', raf, throttle, hold, idle: Number(idle) };
try {
  const got = await until(() => marks.appInfo !== undefined || exit, 120000);
  result.started = Boolean(marks.appInfo);
  result.marks = marks;
  if (marks.appInfo) {
    // At rest: the page is holding; two samples, 4 s apart.
    await wait(1500);
    const first = sample(child.pid);
    const t0 = Date.now();
    await wait(4000);
    const second = sample(child.pid);
    const seconds = (Date.now() - t0) / 1000;
    result.rest = {
      rssMiB: Math.round(second.rss / 1048576),
      privateMiB: Math.round(second.privateBytes / 1048576),
      peakRssMiB: Math.round(second.peak / 1048576),
      cpuPercentOfOneCore: Math.round(((second.cpu - first.cpu) / seconds) * 1000) / 10,
      hasWindow: first.window || second.window,
    };
  } else if (!got) {
    result.problem = 'no app.info within 120 s';
  }
  await until(() => exit, hold + 30000);
  result.exit = exit;
} finally {
  if (!exit) child.kill();
  rmSync(scratch, { recursive: true, force: true });
}
const find = text => lines.find(line => line.includes(text));
result.lines = {
  appInfo: find('HEADLESS app.info'), echo: find('HEADLESS echo'), fetch: find('HEADLESS fetch'),
  timers: find('HEADLESS timers'), window: find('HEADLESS window.state'), done: find('HEADLESS done'),
  failed: lines.filter(line => /HEADLESS page-failed|panicked|software context|make_current|Cannot create/.test(line)),
};
console.log(JSON.stringify(result, null, 2));
process.exitCode = result.started && exit?.code === 0 ? 0 : 1;
void mkdirSync;
void dirname;
