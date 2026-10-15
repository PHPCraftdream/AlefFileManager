// SPDX-License-Identifier: MIT OR Apache-2.0
// Helpers of the end-to-end runner: a scenario site (application files + the transpiled
// @alef-runtime/api), the runtime process with its stderr log, and small local HTTP servers.
import { spawn, spawnSync } from 'node:child_process';
import { copyFileSync, cpSync, existsSync, readFileSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { createServer } from 'node:http';
import { createRequire } from 'node:module';
import { homedir } from 'node:os';
import { dirname, extname, join, normalize, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';

export const here = dirname(fileURLToPath(import.meta.url));
export const root = resolve(here, '..', '..');
export const scratch = join(root, 'backend', 'target', `e2e-${process.pid}`);
/** Where the runtime keeps the data of the application of this id: the folder `$APPDATA` names. */
export function appDataOf(id) {
  const home = homedir();
  if (process.platform === 'win32') return join(process.env.LOCALAPPDATA ?? join(home, 'AppData', 'Local'), id);
  if (process.platform === 'darwin') return join(home, 'Library', 'Application Support', id);
  return join(process.env.XDG_DATA_HOME ?? join(home, '.local', 'share'), id);
}

export const exeName = process.platform === 'win32' ? '.exe' : '';

/**
 * Checks the stall diagnostics of a run against allowances. An allowance matches AT MOST `count`
 * stalls of its kind in `minMs`-`maxMs` inside the `after`/`before` phase markers; with the
 * default `required: false` fewer matches are fine, but any matching stall beyond `count` stays
 * unexpected. `required: true` restores the exact semantics (fewer than `count` is a problem),
 * used only for the deliberate negative control around `e2e.blockBus`, which must fire.
 */
export function busStallProblems(lines, expected = []) {
  const problems = [];
  const actual = [];
  for (let index = 0; index < lines.length; index++) {
    const line = lines[index];
    if (!line.includes('ALEF_BUS_STALL')) continue;
    const match = /^ALEF_BUS_STALL (\S+) (\d+)$/.exec(line);
    if (!match) problems.push(`malformed bus stall diagnostic: ${line}`);
    else actual.push({ kind: match[1], ms: Number(match[2]), index });
  }
  const consumed = new Set();
  for (const allowance of expected) {
    const { kind, minMs, maxMs, count = 1, after, before, required = false } = allowance;
    const afterIndex = after === undefined ? -1 : lines.indexOf(after);
    const beforeIndex = before === undefined ? lines.length : lines.indexOf(before);
    if ((after !== undefined && afterIndex < 0) || (before !== undefined && beforeIndex < 0) || afterIndex >= beforeIndex) {
      problems.push(`missing or invalid bus stall phase markers for ${kind}`);
      continue;
    }
    const matches = actual.filter(item => !consumed.has(item.index) && item.kind === kind
      && item.ms >= minMs && item.ms <= maxMs && item.index > afterIndex && item.index < beforeIndex);
    const want = required ? count : 0;
    for (const item of matches.slice(0, count)) consumed.add(item.index);
    if (matches.length < want) problems.push(`expected ${count} bus stall(s) of ${kind} in ${minMs}-${maxMs}ms; found ${matches.length}`);
  }
  for (const item of actual) {
    if (!consumed.has(item.index)) problems.push(`unexpected bus stall: ${item.kind} ${item.ms}ms`);
  }
  return problems;
}

/**
 * Nobody should have to watch a test: the runtime never shows its windows (`ALEF_E2E_QUIET=1`:
 * geometry, events and the rest are real, visibility and focus are the runtime's own bookkeeping).
 * Linux defaults to a virtual display (`xvfb`) with real windows; `ALEF_E2E_QUIET=1` overrides it.
 * `ALEF_E2E_VISIBLE=1` always shows them and disables software rendering.
 */
export const quiet = process.env.ALEF_E2E_VISIBLE !== '1'
  && (process.platform !== 'linux' || process.env.ALEF_E2E_QUIET === '1');

let activeScenario = null;
let transpiled;

/** Runs one scenario with process-scoped stderr stall enforcement. Scenarios must not overlap. */
export async function runScenario(body) {
  if (activeScenario) throw new Error('concurrent E2E scenarios are not allowed');
  const scope = { records: new Set() };
  activeScenario = scope;
  const problems = [];
  let outcome;
  try {
    outcome = await body();
  } catch (error) {
    problems.push(error?.message ?? String(error));
  } finally {
    for (const record of scope.records) {
      try {
        if (!record.exit) record.stop();
        if (!(await record.waitForClosed(5000))) problems.push('runtime log pipe did not close within 5000ms');
      } catch (error) {
        problems.push(`could not close runtime log pipe: ${error?.message ?? error}`);
      }
    }
    for (const record of scope.records) {
      try { problems.push(...busStallProblems(record.stderrLines, record.expectedBusStalls)); }
      catch (error) { problems.push(`could not evaluate bus stall diagnostics: ${error?.message ?? error}`); }
    }
    activeScenario = null;
  }
  return { lines: [], ...(outcome ?? {}), problems: [...(outcome?.problems ?? []), ...problems] };
}


/** Emits @alef-runtime/api as plain ES modules (`src/`, `types/`); done once per run. */
export function transpileApi() {
  if (transpiled) return transpiled;
  const out = join(scratch, 'api');
  const api = join(root, 'packages', 'api');
  const tsc = join(dirname(createRequire(import.meta.url).resolve('typescript/package.json')), 'bin', 'tsc');
  const result = spawnSync(process.execPath, [
    tsc, '--ignoreConfig', '--strict', '--skipLibCheck', '--target', 'ES2022', '--module', 'ESNext',
    '--moduleResolution', 'bundler', '--rewriteRelativeImportExtensions',
    '--rootDir', api, '--outDir', out, join(api, 'src', 'index.ts'),
  ], { cwd: root, stdio: 'inherit', timeout: 120000 });
  if (result.status !== 0) throw new Error('could not transpile @alef-runtime/api for the scenarios');
  transpiled = out;
  return out;
}

let transpiledMcp;

/** Emits MCP with the packages layout, then redirects its runtime imports to the shared site API. */
export function transpileMcp() {
  if (transpiledMcp) return transpiledMcp;
  const packages = join(root, 'packages');
  const out = join(scratch, 'mcp-packages');
  const tsc = join(dirname(createRequire(import.meta.url).resolve('typescript/package.json')), 'bin', 'tsc');
  const result = spawnSync(process.execPath, [
    tsc, '--ignoreConfig', '--strict', '--skipLibCheck', '--target', 'ES2022', '--module', 'ESNext',
    '--moduleResolution', 'bundler', '--rewriteRelativeImportExtensions',
    '--rootDir', packages, '--outDir', out, join(packages, 'mcp', 'src', 'index.ts'),
  ], { cwd: root, stdio: 'inherit', timeout: 30000 });
  if (result.status !== 0) throw new Error('could not transpile @alef-runtime/mcp for the scenarios');
  const adapter = join(out, 'mcp', 'src', 'transports', 'runtime.js');
  writeFileSync(adapter, readFileSync(adapter, 'utf8').replaceAll('../../../api/src/', '../../../src/'));
  transpiledMcp = join(out, 'mcp');
  return transpiledMcp;
}

/**
 * Builds the directory a scenario runs from: the application files of `apps/<app>` (`{{KEY}}` in
 * alef.ktav replaced), the shared harness, the transpiled API and `targets.json` for the page.
 */
export function prepareSite(name, app, { replacements = {}, targets = {} } = {}) {
  const site = join(scratch, 'sites', name);
  rmSync(site, { recursive: true, force: true });
  const source = join(here, 'apps', app);
  cpSync(source, site, {
    recursive: true,
    // Desktop/system contain independent child apps: never import their trees.
    filter: path => path === source || !statSync(path).isDirectory() || !existsSync(join(path, 'alef.ktav')),
  });
  const publicRoot = join(site, 'dist');
  copyFileSync(join(here, 'harness.js'), join(publicRoot, 'harness.js'));
  const api = transpileApi();
  for (const folder of ['src', 'types']) cpSync(join(api, folder), join(publicRoot, folder), { recursive: true });
  if (app.startsWith('modules/net/mcp')) cpSync(transpileMcp(), join(publicRoot, 'mcp'), { recursive: true });
  const manifest = join(site, 'alef.ktav');
  let text = readFileSync(manifest, 'utf8');
  for (const [key, value] of Object.entries(replacements)) text = text.replaceAll(`{{${key}}}`, value);
  // An application with a fixed id (the core one) takes another id for scenarios that need two origins.
  if (replacements.ID !== undefined && !text.includes(replacements.ID)) text = text.replace(/^id: .*$/m, `id: ${replacements.ID}`);
  writeFileSync(manifest, text);
  writeFileSync(join(publicRoot, 'targets.json'), JSON.stringify({ ...targets, app: site, sep }));
  return site;
}

const quietBinaries = new Map();

/**
 * A runtime that does not know the quiet mode would show its windows to the user and take the
 * focus: such a binary is not started. `ALEF_E2E_VISIBLE=1` accepts visible windows.
 */
export function assertQuiet(exe) {
  const software = process.env.ALEF_E2E_VISIBLE !== '1' && process.env.ALEF_E2E_SOFTWARE === '1';
  if (!quiet && !software) return;
  if (!quietBinaries.has(exe)) {
    const binary = readFileSync(exe);
    quietBinaries.set(exe, {
      quiet: binary.includes('ALEF_E2E_QUIET'),
      software: binary.includes('ALEF_E2E_SOFTWARE'),
    });
  }
  if (quiet && !quietBinaries.get(exe).quiet) {
    throw new Error(`${exe} has no quiet mode, its windows would appear on your screen: build the current sources, or set ALEF_E2E_VISIBLE=1 to accept visible windows`);
  }
  if (software && !quietBinaries.get(exe).software) {
    throw new Error(`${exe} has no ALEF_E2E_SOFTWARE support: build the current sources before requesting software rendering`);
  }
}

/**
 * The user of an end-to-end run has decided before the start: every right is given (a scenario of
 * consent says otherwise), and the decisions are kept in a folder of the run, not in the data of the user.
 */
const defaultDecisions = { ALEF_HOME: join(scratch, 'home'), ALEF_E2E_CONSENT: '*=allow' };

/**
 * Starts the runtime; its stderr and stdout are kept line by line and can be awaited. `input` (text or bytes)
 * is written to its stdin, which is then closed; without it stdin is closed from the start.
 */
export function startApp({ exe, args, env = {}, verbose = false, input, interactive = false, expectedBusStalls = [], undecidedConsent = false }) {
  assertQuiet(exe);
  // `undecidedConsent`: no scripted answer at all, so the real consent window opens (an empty
  // `ALEF_E2E_CONSENT` script would answer nothing and refuse every right without a window).
  // Every inherited consent override is removed BEFORE the scenario env is merged, so only what the
  // scenario passes explicitly (e.g. the UI steps and the hold) reaches the child.
  const childEnv = { ...process.env, ALEF_E2E: '1', RUST_BACKTRACE: '1' };
  if (undecidedConsent) for (const name of ['ALEF_E2E_CONSENT', 'ALEF_E2E_CONSENT_UI', 'ALEF_E2E_CONSENT_HOLD_MS']) delete childEnv[name];
  Object.assign(childEnv, quiet ? { ALEF_E2E_QUIET: '1' } : {}, undecidedConsent ? {} : defaultDecisions, env,
    process.env.ALEF_E2E_VISIBLE === '1' ? { ALEF_E2E_QUIET: '', ALEF_E2E_SOFTWARE: '' } : {});
  const child = spawn(exe, args, {
    env: childEnv,
    stdio: [input === undefined && !interactive ? 'ignore' : 'pipe', 'pipe', 'pipe'],
  });
  if (input !== undefined || interactive) child.stdin.on('error', () => {});
  if (input !== undefined) child.stdin.end(input);
  const output = [];
  const errors = [];
  child.stderr.on('data', chunk => errors.push(Buffer.from(chunk)));
  child.stdout.on('data', chunk => output.push(Buffer.from(chunk)));
  const lines = [];
  const stderrLines = [];
  const watchers = new Set();
  let exit = null;
  const wake = () => [...watchers].forEach(watcher => watcher());
  const record = (raw, targets) => {
    const line = raw.trim();
    if (!line) return;
    // One split state per stream: a stderr line is recorded once and lands in every
    // collector (the ordered log and the stall diagnostics) in the same handler.
    for (const target of targets) target.push(line);
    if (targets.includes(lines)) {
      if (verbose) console.log(`    | ${line}`);
      wake();
    }
  };
  const collect = (stream, targets) => {
    let buffer = '';
    stream.on('data', chunk => {
      buffer += chunk;
      const parts = buffer.split('\n');
      buffer = parts.pop();
      for (const raw of parts) record(raw, targets);
    });
    stream.on('end', () => {
      if (buffer) record(buffer, targets);
    });
  };
  collect(child.stderr, [lines, stderrLines]);
  collect(child.stdout, [lines]);
  child.on('exit', code => {
    exit = { code };
    wake();
  });
  // Every holder of the pipe is gone: a program the runtime started again has finished as well.
  const closed = new Promise(resolvePromise => child.stderr.on('close', resolvePromise));
  // The stdout pipe is closed only when the runtime and everything holding it are done: reading
  // `stdout()` before that loses the tail of a large stream (the drain race).
  const stdoutClosed = new Promise(resolvePromise => child.stdout.on('close', resolvePromise));
  const processRecord = {
    stderrLines, expectedBusStalls, stop: () => child.kill(),
    waitForClosed: ms => new Promise(resolvePromise => {
      let done = false;
      const timer = setTimeout(() => { done = true; resolvePromise(false); }, ms);
      closed.then(() => {
        if (done) return;
        clearTimeout(timer);
        done = true;
        resolvePromise(true);
      });
    }),
    get exit() { return exit; },
  };
  if (activeScenario) activeScenario.records.add(processRecord);
  const waitFor = (predicate, ms, what, { survivesExit = false } = {}) => new Promise((resolvePromise, reject) => {
    const timer = setTimeout(() => {
      watchers.delete(look);
      reject(new Error(`timed out waiting for ${what}`));
    }, ms);
    function look() {
      const found = lines.find(predicate);
      if (found !== undefined) {
        clearTimeout(timer);
        watchers.delete(look);
        resolvePromise(found);
      } else if (exit && !survivesExit) {
        clearTimeout(timer);
        watchers.delete(look);
        reject(new Error(`the runtime exited (${exit.code}) while waiting for ${what}; last output: ${lines.slice(-6).join(' | ')}`));
      }
    }
    watchers.add(look);
    look();
  });
  return {
    lines,
    /** Everything the runtime wrote to its stdout, as bytes. */
    stdout: () => Buffer.concat(output),
    /** Everything the runtime and its descendants wrote to stderr, as bytes. */
    stderr: () => Buffer.concat(errors),
    waitFor,
    waitForExit: ms => waitFor(() => false, ms, 'the runtime to exit').catch(() => exit),
    /** Resolves `true` once nobody holds the log pipe any more, `false` after `ms`. */
    waitForClosed: processRecord.waitForClosed,
    /** Resolves `true` once the stdout pipe is closed, `false` after `ms`: await before `stdout()`. */
    stdoutDrained: ms => new Promise(resolvePromise => {
      let done = false;
      const timer = setTimeout(() => { done = true; resolvePromise(false); }, ms);
      stdoutClosed.then(() => {
        if (done) return;
        clearTimeout(timer);
        done = true;
        resolvePromise(true);
      });
    }),
    /** Stall diagnostics from stderr only, for direct startApp users outside runScenario. */
    busProblems: () => busStallProblems(stderrLines, expectedBusStalls),
    /** Writes to the stdin of an `interactive` run; `endInput` closes it. */
    write: text => child.stdin.write(text),
    endInput: () => child.stdin.end(),
    stop: () => child.kill(),
    pid: child.pid,
    get exit() { return exit; },
  };
}

const TYPES = { '.html': 'text/html', '.js': 'text/javascript', '.json': 'application/json', '.txt': 'text/plain' };

/** Serves `handler(request, response)` on a free 127.0.0.1 port. */
export async function listen(handler) {
  const server = createServer(handler);
  await new Promise(resolvePromise => server.listen(0, '127.0.0.1', resolvePromise));
  const { port } = server.address();
  return {
    port,
    origin: `http://127.0.0.1:${port}`,
    close: () => new Promise(resolvePromise => { server.closeAllConnections?.(); server.close(resolvePromise); }),
  };
}

/** A server that records the paths asked for and answers with permissive CORS. */
export async function countingServer() {
  const hits = [];
  const server = await listen((request, response) => {
    hits.push(`${request.method} ${request.url}`);
    response.writeHead(200, { 'content-type': 'text/plain', 'access-control-allow-origin': '*' });
    response.end('pong');
  });
  return { ...server, hits };
}

/** Serves the files of `directory` the way a development server would. */
export async function staticServer(directory) {
  const requests = [];
  const base = resolve(directory);
  const server = await listen((request, response) => {
    requests.push(`${request.method} ${request.url}`);
    const path = normalize(join(base, decodeURIComponent(new URL(request.url, 'http://x').pathname)));
    let file = path;
    try {
      if (statSync(path).isDirectory()) file = join(path, 'index.html');
    } catch {
      // answered below
    }
    if (!file.startsWith(base)) {
      response.writeHead(403).end();
      return;
    }
    try {
      const bytes = readFileSync(file);
      response.writeHead(200, { 'content-type': TYPES[extname(file)] ?? 'application/octet-stream' });
      response.end(bytes);
    } catch {
      response.writeHead(404).end();
    }
  });
  return { ...server, requests };
}

/** Lines worth showing when a scenario fails. */
export const interesting = lines => lines.filter(line => /FAILED|PROBLEM|fatal|Servo Error|panicked|error/i.test(line));

export const field = (line, key) => Number(new RegExp(`${key}=(\\d+)`).exec(line)?.[1]);
const okNames = lines => new Set(lines.map(line => /check (\S+) ok /.exec(line)?.[1]).filter(Boolean));
export const verdictOf = lines => /ALEF_E2E RESULT (PASS|FAIL)\s*(.*)$/.exec(lines.find(line => line.includes('ALEF_E2E RESULT')) ?? '');
const missing = (lines, expected) => expected.filter(name => !okNames(lines).has(name));

/**
 * The page-driven scenario runner bound to one runtime binary: builds the site, starts the runtime,
 * waits for the page's verdict and returns the problems found (empty = pass). `judge(lines, result,
 * running)` adds scenario-specific checks and may wait for the runtime to exit.
 */
export function makeDriver({ exe, verbose, timeoutMs }) {
  return async function drive({ name, app, replacements, targets, env, args: extra = [], expectedChecks, expectedBusStalls = [], judge, appArgs, input }) {
    const site = prepareSite(name, app, { replacements, targets });
    const running = startApp({ exe, args: appArgs ?? ['--app', site, ...extra], env, verbose, input, expectedBusStalls });
    const problems = [];
    try {
      await running.waitFor(line => line.includes('ALEF_E2E RESULT'), timeoutMs, 'the verdict of the page');
      const result = verdictOf(running.lines);
      if (!result) problems.push('no verdict line');
      else if (!env?.ALEF_E2E_BREAK) {
        if (result[1] !== 'PASS') problems.push(`verdict ${result[1]}: ${result[2]}`);
        const absent = missing(running.lines, expectedChecks);
        if (absent.length > 0) problems.push(`checks without an ok line: ${absent}`);
      }
      problems.push(...(await judge?.(running.lines, result, running) ?? []));
    } catch (error) {
      problems.push(error.message);
    } finally {
      running.stop();
      if (!(await running.waitForClosed(5000))) problems.push('runtime log pipe did not close within 5000ms');
    }
    return { problems, lines: running.lines, site, expectedBusStalls };
  };
}
