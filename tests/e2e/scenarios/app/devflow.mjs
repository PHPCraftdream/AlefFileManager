// SPDX-License-Identifier: MIT OR Apache-2.0
// The `devflow` scenario: the developer path on a clean machine. Node and the create-alef-runtime
// CLI generate a project, unpublished workspace packages arrive as npm tarballs, and the installed
// @alef-runtime/tools CLI builds, keys, packs and verifies it; the real `alef` binary runs the
// package and the dev flow. No cargo is invoked anywhere in the scenario.
import { spawn, spawnSync } from 'node:child_process';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { createHash } from 'node:crypto';
import os from 'node:os';
import { join, resolve } from 'node:path';

import { quiet, root, startApp } from '../../lib.mjs';

const ID = 'org.alef.e2e.devflow';
const NAME = 'Alef e2e devflow';
const CREATE_BIN = join(root, 'packages', 'create-alef-runtime', 'bin', 'create-alef-runtime.mjs');

/** A bounded Node child; stdout+stderr as text. `shell` only where npm needs cmd.exe on Windows. */
const run = (command, args, { cwd, env = {}, timeoutMs, shell = false }) => {
  const result = spawnSync(command, args, {
    cwd, timeout: timeoutMs, encoding: 'utf8', shell, windowsHide: true,
    env: { ...process.env, ...env },
  });
  return { code: result.status, out: result.stdout ?? '', err: result.stderr ?? '' };
};

// npm is npm.cmd on Windows: a shell joins the line there; the paths contain no cmd metacharacters.
const npm = (args, options) => run('npm', args, { ...options, shell: process.platform === 'win32' });

/** Collects the lines of a long-running Node child; bounded waits, kill on deadline. */
const collect = (args, { cwd, env = {}, verbose = false }) => {
  const child = spawn(process.execPath, args, { cwd, env: { ...process.env, ...env }, stdio: ['ignore', 'pipe', 'pipe'], windowsHide: true });
  const lines = [];
  for (const stream of [child.stdout, child.stderr]) {
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
    });
  }
  let exit = null;
  child.on('exit', code => { exit = { code }; });
  // Bounded stop: on Windows the tree dies via taskkill, elsewhere SIGTERM then SIGKILL; never hangs.
  const stop = async () => {
    if (child.exitCode !== null || exit !== null) return;
    await new Promise(resolvePromise => {
      if (process.platform === 'win32') {
        const killer = spawn('taskkill.exe', ['/PID', String(child.pid), '/T', '/F'], { stdio: 'ignore', windowsHide: true });
        const timer = setTimeout(() => { killer.kill(); resolvePromise(); }, 5000);
        killer.once('error', () => { clearTimeout(timer); resolvePromise(); });
        killer.once('close', () => { clearTimeout(timer); resolvePromise(); });
      } else {
        try { child.kill(); } catch { /* already gone */ }
        const timer = setTimeout(() => { try { child.kill('SIGKILL'); } catch { /* already gone */ } resolvePromise(); }, 3000);
        child.once('exit', () => { clearTimeout(timer); resolvePromise(); });
      }
    });
  };
  return {
    lines,
    waitFor: (predicate, ms, what) => new Promise((resolvePromise, reject) => {
      const deadline = Date.now() + ms;
      const look = () => {
        const found = lines.find(predicate);
        if (found !== undefined) { resolvePromise(found); return; }
        if (exit !== null) { reject(new Error(`the dev process exited (${exit.code}) while waiting for ${what}`)); return; }
        if (Date.now() > deadline) { reject(new Error(`timed out waiting for ${what}`)); return; }
        setTimeout(look, 100);
      };
      look();
    }),
    waitForExit: ms => new Promise(resolvePromise => {
      const deadline = Date.now() + ms;
      const look = () => {
        if (exit !== null) { resolvePromise(exit); return; }
        if (Date.now() > deadline) { stop(); resolvePromise(null); return; }
        setTimeout(look, 100);
      };
      look();
    }),
    stop,
  };
};

// The e2e tail: the page proves the transport works, reports like tests/e2e/harness.js and exits.
const PAGE_TAIL = `
// e2e (devflow): fail when the generated page itself failed, report the serving origin
// (dev server vs package origin) and exit, like tests/e2e/harness.js does.
import { call } from '@alef-runtime/api/core';
try {
  await app.info();
} catch (error) {
  await call('e2e.report', { line: \`RESULT FAIL app-info \${error?.message ?? error}\` });
  await app.exit(1);
}
await call('e2e.report', { line: 'devflow-origin=' + location.origin });
await call('e2e.report', { line: 'RESULT PASS devflow-page' });
await app.exit(0);
`;

export function devflowScenarios({ exe, verbose, timeoutMs }) {
  // CI passes a relative --exe; the tools CLI and the runtime must get one absolute path.
  const exeAbs = resolve(exe);
  return {
    devflow: async () => {
      const problems = [];
      const expect = (what, condition, detail = '') => {
        console.log(`    ${condition ? 'ok  ' : 'FAIL'} ${what}${condition ? '' : `: ${detail}`}`);
        if (!condition) problems.push(`${what}: ${detail}`);
      };
      // XDG bases into the scratch dir on Linux, exactly like the package scenario.
      const dir = mkdtempSync(join(os.tmpdir(), 'alef-e2e-devflow-'));
      const project = join(dir, 'app');
      const tarballs = join(dir, 'tarballs');
      const keyHome = join(dir, 'key-home');
      const toolEnv = { ALEF_RUNTIME_HOME: keyHome };
      const xdg = () => process.platform === 'linux'
        ? { XDG_DATA_HOME: join(dir, 'xdg', 'data'), XDG_CONFIG_HOME: join(dir, 'xdg', 'config'), XDG_CACHE_HOME: join(dir, 'xdg', 'cache') }
        : {};
      // The application profile must never land outside the scratch dir, whatever the
      // environment inherits: ALEF_APP_HOME moves the data/config/cache bases of the app,
      // and on Windows the OS appdata of the children goes to the scratch dir too.
      const appHome = join(dir, 'app-home');
      const isolation = () => ({
        ALEF_APP_HOME: appHome,
        ...(process.platform === 'win32' ? { APPDATA: join(dir, 'appdata'), LOCALAPPDATA: join(dir, 'appdata') } : {}),
        ...xdg(),
      });
      let runLines = [];
      let devLines = [];
      let dev;
      try {
        // (a) generate
        const created = run(process.execPath, [CREATE_BIN, '--yes', '--name', NAME, '--id', ID, '--bundler', 'rsbuild', 'app'], { cwd: dir, timeoutMs: 60000 });
        expect('create-alef-runtime generates the project', created.code === 0 && existsSync(join(project, 'package.json')), `${created.code}: ${created.err}`);
        if (problems.length > 0) return { problems, lines: [] };
        expect('the generated manifest names the fixed id', readFileSync(join(project, 'alef.ktav'), 'utf8').includes(`id: ${ID}`), join(project, 'alef.ktav'));

        // (b) pack the unpublished workspace packages and point the project at the tarballs
        mkdirSync(tarballs, { recursive: true });
        // The api tarball ships dist/ (API-4): build the package before it is packed.
        const apiBuild = run(process.execPath, [join(root, 'packages', 'api', 'build.mjs')], { cwd: dir, timeoutMs: 240000 });
        expect('the @alef-runtime/api package builds its dist', apiBuild.code === 0, `${apiBuild.code}: ${apiBuild.err.slice(-400)}`);
        const packed = {};
        for (const name of ['api', 'tools']) {
          const result = npm(['pack', '--pack-destination', tarballs, join(root, 'packages', name)], { cwd: dir, timeoutMs: 120000 });
          const file = result.out.trim().split('\n').pop()?.trim();
          expect(`npm pack builds the @alef-runtime/${name} tarball`, result.code === 0 && file !== undefined && file.endsWith('.tgz'), `${result.code}: ${result.err}`);
          if (file) packed[name] = file;
        }
        if (!packed.api || !packed.tools) return { problems, lines: [] };
        const manifestPath = join(project, 'package.json');
        const pkg = JSON.parse(readFileSync(manifestPath, 'utf8'));
        pkg.dependencies['@alef-runtime/api'] = `file:../tarballs/${packed.api}`;
        pkg.devDependencies['@alef-runtime/tools'] = `file:../tarballs/${packed.tools}`;
        writeFileSync(manifestPath, JSON.stringify(pkg, null, 2));
        // The runner's normal npm cache (setup-node restores it); only the userconfig stays in scratch.
        const install = npm(['install', '--no-audit', '--no-fund', '--no-progress', '--loglevel', 'error',
          '--userconfig', join(dir, 'npmrc')], { cwd: project, timeoutMs: 600000 });
        expect('npm install resolves the tarball dependencies', install.code === 0
          && existsSync(join(project, 'node_modules', '@alef-runtime', 'tools', 'bin', 'alef-runtime.mjs')), `${install.code}: ${install.err.slice(-400)}`);

        // The page tail goes in before the build: both the package and the dev run report and exit.
        const mainJs = join(project, 'src', 'main.js');
        writeFileSync(mainJs, readFileSync(mainJs, 'utf8') + PAGE_TAIL);

        // (c) build, keys, pack, verify — all through the installed CLI
        const toolsBin = join(project, 'node_modules', '@alef-runtime', 'tools', 'bin', 'alef-runtime.mjs');
        const build = run(process.execPath, [toolsBin, 'build'], { cwd: project, env: toolEnv, timeoutMs: 600000 });
        const appDir = join(project, 'app-build');
        expect('alef-runtime build assembles the app directory', build.code === 0
          && existsSync(join(appDir, 'alef.ktav')) && existsSync(join(appDir, 'dist', 'index.html')), `${build.code}: ${build.err.slice(-400)}`);
        const keys = run(process.execPath, [toolsBin, 'keys', 'e2e-devflow'], { cwd: project, env: toolEnv, timeoutMs: 60000 });
        const fingerprint = keys.out.split('\n')[0]?.trim().split(' ')[1];
        expect('alef-runtime keys creates a key under ALEF_RUNTIME_HOME', keys.code === 0 && /^[0-9a-f]{64}$/.test(fingerprint ?? ''), `${keys.code}: ${keys.err}`);
        const pkgPath = join(dir, 'devflow.alef');
        const pack = run(process.execPath, [toolsBin, 'pack', 'app-build', '--key', 'e2e-devflow', '--out', pkgPath], { cwd: project, env: toolEnv, timeoutMs: 60000 });
        expect('alef-runtime packs the built app', pack.code === 0 && existsSync(pkgPath), `${pack.code}: ${pack.err}`);
        const verify = run(process.execPath, [toolsBin, 'verify', pkgPath], { cwd: project, env: toolEnv, timeoutMs: 60000 });
        expect('alef-runtime verify names the signing fingerprint', verify.code === 0 && verify.out.includes(`signed by ${fingerprint}`), `${verify.code}: ${verify.out} ${verify.err}`);

        // (d) alef run of the package with the trust key
        const homeRun = join(dir, 'home-run');
        const running = startApp({ exe: exeAbs, verbose, args: ['run', pkgPath, '--trust-key', fingerprint],
          env: { ALEF_HOME: homeRun, ALEF_RUNTIME_HOME: keyHome, ALEF_E2E_CONSENT: '*=allow', ...isolation() } });
        try {
          await running.waitFor(line => line.includes('ALEF_READY'), timeoutMs, 'ALEF_READY');
          await running.waitFor(line => line.includes('ALEF_E2E RESULT'), timeoutMs, 'the verdict of the page');
          const exit = await running.waitForExit(timeoutMs);
          runLines = running.lines;
          expect('the packaged app starts, its page reports and exits cleanly', exit?.code === 0, `exit ${exit?.code}`);
          expect('the page verdict is PASS', runLines.some(line => line.includes('ALEF_E2E RESULT PASS')), runLines.find(line => line.includes('ALEF_E2E RESULT')) ?? 'no verdict');
          const pinPath = join(homeRun, 'trust', `${createHash('sha256').update(ID).digest('hex')}.json`);
          expect('the trust key was pinned under the scratch home', existsSync(pinPath) && readFileSync(pinPath, 'utf8').includes(fingerprint), pinPath);
          expect('the key file exists under the scratch ALEF_RUNTIME_HOME', existsSync(join(keyHome, 'keys', 'e2e-devflow.pem')), join(keyHome, 'keys', 'e2e-devflow.pem'));
          const runOrigin = runLines.find(line => line.includes('devflow-origin='));
          expect('the packaged document is served from its package origin, not the dev server', runOrigin !== undefined
            && runOrigin.includes('.alef') && !runOrigin.includes('127.0.0.1'), runOrigin ?? 'no origin line');
        } catch (error) {
          expect('the packaged app starts, its page reports and exits cleanly', false, error.message);
        } finally {
          running.stop();
          await running.waitForExit(15000);
        }

        // (e) alef-runtime dev: dev server + runtime, both bounded; the page tail exits the runtime,
        // the CLI stops the dev server and resolves with the runtime exit code.
        dev = collect([toolsBin, 'dev', '--ready-timeout', '60000'],
          { cwd: project, verbose, env: { ALEF_BIN: exeAbs, ALEF_HOME: join(dir, 'home-dev'), ALEF_RUNTIME_HOME: keyHome, ALEF_E2E: '1', ALEF_E2E_CONSENT: '*=allow', ...(quiet ? { ALEF_E2E_QUIET: '1' } : {}), ...isolation() } });
        try {
          await dev.waitFor(line => line.includes('ALEF_READY'), timeoutMs, 'the dev runtime to be ready');
          await dev.waitFor(line => line.includes('ALEF_E2E RESULT'), timeoutMs, 'the verdict of the dev page');
          const exit = await dev.waitForExit(timeoutMs);
          devLines = dev.lines;
          expect('alef-runtime dev serves the page through the runtime and shuts both down', exit?.code === 0, `exit ${exit?.code}`);
          expect('the dev page verdict is PASS', devLines.some(line => line.includes('ALEF_E2E RESULT PASS')), devLines.find(line => line.includes('ALEF_E2E RESULT')) ?? 'no verdict');
          const devOrigin = devLines.find(line => line.includes('devflow-origin='));
          expect('the dev document came from the dev server', devOrigin !== undefined
            && devOrigin.includes('http://127.0.0.1:3000'), devOrigin ?? 'no origin line');
        } catch (error) {
          devLines = dev.lines;
          const tail = verbose ? '' : `; last output: ${devLines.slice(-40).join(' | ')}`;
          // Whether the dev server still answers tells a page that never loaded from a server that died.
          const server = await fetch('http://127.0.0.1:3000/', { signal: AbortSignal.timeout(5000) }).then(
            async response => `the dev server answers ${response.status} with ${(await response.text()).length} bytes`,
            reason => `the dev server does not answer: ${reason.message}`);
          expect('alef-runtime dev serves the page through the runtime and shuts both down', false, `${error.message}; ${server}${tail}`);
        } finally {
          await dev.stop();
        }
        return { problems, lines: [...runLines, ...devLines] };
      } finally {
        if (dev) await dev.stop();
        rmSync(dir, { recursive: true, force: true });
      }
    },
  };
}
