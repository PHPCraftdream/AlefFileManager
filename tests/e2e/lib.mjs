// SPDX-License-Identifier: MIT OR Apache-2.0
// Helpers of the end-to-end runner: a scenario site (application files + the transpiled
// @alef-tron/api), the runtime process with its stderr log, and small local HTTP servers.
import { spawn, spawnSync } from 'node:child_process';
import { copyFileSync, cpSync, readFileSync, rmSync, statSync, writeFileSync } from 'node:fs';
import { createServer } from 'node:http';
import { createRequire } from 'node:module';
import { dirname, extname, join, normalize, resolve, sep } from 'node:path';
import { fileURLToPath } from 'node:url';

export const here = dirname(fileURLToPath(import.meta.url));
export const root = resolve(here, '..', '..');
export const scratch = join(root, 'backend', 'target', `e2e-${process.pid}`);
export const exeName = process.platform === 'win32' ? '.exe' : '';

let transpiled;

/** Emits @alef-tron/api as plain ES modules (`src/`, `types/`); done once per run. */
export function transpileApi() {
  if (transpiled) return transpiled;
  const out = join(scratch, 'api');
  const api = join(root, 'packages', 'api');
  const tsc = join(dirname(createRequire(import.meta.url).resolve('typescript/package.json')), 'bin', 'tsc');
  const result = spawnSync(process.execPath, [
    tsc, '--ignoreConfig', '--strict', '--skipLibCheck', '--target', 'ES2022', '--module', 'ESNext',
    '--moduleResolution', 'bundler', '--rewriteRelativeImportExtensions',
    '--rootDir', api, '--outDir', out, join(api, 'src', 'index.ts'),
  ], { cwd: root, stdio: 'inherit' });
  if (result.status !== 0) throw new Error('could not transpile @alef-tron/api for the scenarios');
  transpiled = out;
  return out;
}

/**
 * Builds the directory a scenario runs from: the application files of `apps/<app>` (`{{KEY}}` in
 * alef.ktav replaced), the shared harness, the transpiled API and `targets.json` for the page.
 */
export function prepareSite(name, app, { replacements = {}, targets = {} } = {}) {
  const site = join(scratch, 'sites', name);
  rmSync(site, { recursive: true, force: true });
  cpSync(join(here, 'apps', app), site, { recursive: true });
  copyFileSync(join(here, 'harness.js'), join(site, 'harness.js'));
  const api = transpileApi();
  for (const folder of ['src', 'types']) cpSync(join(api, folder), join(site, folder), { recursive: true });
  const manifest = join(site, 'alef.ktav');
  let text = readFileSync(manifest, 'utf8');
  for (const [key, value] of Object.entries(replacements)) text = text.replaceAll(`{{${key}}}`, value);
  writeFileSync(manifest, text);
  writeFileSync(join(site, 'targets.json'), JSON.stringify({ app: site, sep, ...targets }));
  return site;
}

/** Starts the runtime; its stderr and stdout are kept line by line and can be awaited. */
export function startApp({ exe, args, env = {}, verbose = false }) {
  const child = spawn(exe, args, {
    env: { ...process.env, ALEF_E2E: '1', ...env },
    stdio: ['ignore', 'pipe', 'pipe'],
  });
  const lines = [];
  const watchers = new Set();
  let exit = null;
  const wake = () => [...watchers].forEach(watcher => watcher());
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
        if (verbose) console.log(`    | ${line}`);
      }
      wake();
    });
  };
  collect(child.stderr);
  collect(child.stdout);
  child.on('exit', code => {
    exit = { code };
    wake();
  });
  // Every holder of the pipe is gone: a program the runtime started again has finished as well.
  const closed = new Promise(resolvePromise => child.stderr.on('close', resolvePromise));
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
        reject(new Error(`the runtime exited (${exit.code}) while waiting for ${what}`));
      }
    }
    watchers.add(look);
    look();
  });
  return {
    lines,
    waitFor,
    waitForExit: ms => waitFor(() => false, ms, 'the runtime to exit').catch(() => exit),
    /** Resolves `true` once nobody holds the log pipe any more, `false` after `ms`. */
    waitForClosed: ms => Promise.race([closed.then(() => true), new Promise(resolvePromise => setTimeout(resolvePromise, ms, false))]),
    stop: () => child.kill(),
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
      response.writeHead(200, { 'content-type': TYPES[extname(file)] ?? 'application/octet-stream' });
      response.end(readFileSync(file));
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
  return async function drive({ name, app, replacements, targets, env, args: extra = [], expectedChecks, judge, appArgs }) {
    const site = prepareSite(name, app, { replacements, targets });
    const running = startApp({ exe, args: appArgs ?? ['--app', site, ...extra], env, verbose });
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
    }
    return { problems, lines: running.lines, site };
  };
}
