// SPDX-License-Identifier: MIT OR Apache-2.0
// The `devflow` scenario: the developer path on a clean machine. Node and the create-alef-runtime
// CLI generate a project, unpublished workspace packages arrive as npm tarballs, and the installed
// @alef-runtime/tools CLI builds, keys, packs and verifies it; the real `alef` binary runs the
// package and the dev flow. No cargo is invoked anywhere in the scenario.
// The file also carries the `devtools` scenario (the raw remote-debugging protocol of
// `alef --app <dir> --devtools[=PORT]`): a plain TCP client speaks the auth handshake, a wrong
// token is refused, the default port stays closed without the flag, a busy port is refused with
// exit 1, and a packaged application refuses the flag entirely.
import { spawn, spawnSync } from 'node:child_process';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { createHash } from 'node:crypto';
import net from 'node:net';
import os from 'node:os';
import { join, resolve } from 'node:path';

import { quiet, prepareSite, root, scratch, startApp } from '../../lib.mjs';
import { pack } from '../../../../packages/tools/src/index.mjs';

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
        const packResult = run(process.execPath, [toolsBin, 'pack', 'app-build', '--key', 'e2e-devflow', '--out', pkgPath], { cwd: project, env: toolEnv, timeoutMs: 60000 });
        expect('alef-runtime packs the built app', packResult.code === 0 && existsSync(pkgPath), `${packResult.code}: ${packResult.err}`);
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
          const probe = url => fetch(url, { signal: AbortSignal.timeout(5000) }).then(
            async response => ({ status: response.status, text: await response.text() }),
            reason => ({ failure: reason.message }));
          const landing = await probe('http://127.0.0.1:3000/');
          let server = landing.failure === undefined
            ? `the dev server answers ${landing.status} with ${landing.text.length} bytes`
            : `the dev server does not answer: ${landing.failure}`;
          if (landing.failure === undefined) {
            // A page that loaded without a single console line never ran its scripts: say what each one serves.
            const sources = [...landing.text.matchAll(/<script[^>]*\ssrc="([^"]+)"/g)].map(found => found[1]).slice(0, 6);
            const served = [];
            for (const source of sources) {
              const script = await probe(new URL(source, 'http://127.0.0.1:3000/').href);
              served.push(`${source} -> ${script.failure ?? `${script.status}, ${script.text.length} bytes`}`);
            }
            server += `; scripts: ${served.join(', ') || 'none in the document'}`;
          }
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

// The `devtools` scenario lives here too (the scenarios directory has a file-count budget),
// sharing the module's helpers and imports.

/** Bounded wait: the promise, or a rejection naming `what` after `ms`. */
const withTimeout = (promise, ms, what) => new Promise((resolvePromise, reject) => {
  const timer = setTimeout(() => reject(new Error(`timed out after ${ms}ms waiting for ${what}`)), ms);
  promise.then(
    value => { clearTimeout(timer); resolvePromise(value); },
    error => { clearTimeout(timer); reject(error); },
  );
});

/** A free 127.0.0.1 port: bind, read, close (the usual small race is accepted). */
const freePort = () => new Promise((resolvePromise, reject) => {
  const server = net.createServer();
  server.once('error', reject);
  server.listen(0, '127.0.0.1', () => {
    const { port } = server.address();
    server.close(() => resolvePromise(port));
  });
});

/** Holds `port` until `close()` is awaited: the busy port of sub-check D. */
const holdPort = port => new Promise((resolvePromise, reject) => {
  const server = net.createServer();
  server.once('error', reject);
  server.listen(port, '127.0.0.1', () => resolvePromise({
    close: () => new Promise(resolveClose => server.close(resolveClose)),
  }));
});

const connect = port => new Promise((resolvePromise, reject) => {
  const socket = net.connect(port, '127.0.0.1');
  socket.once('error', reject);
  socket.once('connect', () => {
    socket.removeListener('error', reject);
    // Late failures must not crash the process; the checks read the outcome instead.
    socket.on('error', () => {});
    resolvePromise(socket);
  });
});

/** The exact auth packet of the protocol: a length-prefixed JSON object with the token. */
const authPacket = token => {
  const payload = JSON.stringify({ auth_token: token });
  return Buffer.from(`${Buffer.byteLength(payload)}:${payload}`);
};

/** Waits for the socket to close; on timeout destroys it and resolves `false`. */
const closedWithin = async (socket, ms) => {
  try {
    await withTimeout(new Promise(resolvePromise => socket.once('close', resolvePromise)), ms, 'the server to close the socket');
    return true;
  } catch {
    socket.destroy();
    return false;
  }
};

/** Reads one length-prefixed JSON frame from the socket; bounded by time and buffer size. */
const readFrame = (socket, { timeoutMs, maxBytes }) => new Promise((resolvePromise, reject) => {
  let buffer = Buffer.alloc(0);
  let done = false;
  const finish = (error, frame) => {
    if (done) return;
    done = true;
    socket.removeListener('data', onData);
    clearTimeout(timer);
    if (error) reject(error); else resolvePromise(frame);
  };
  const onData = chunk => {
    buffer = Buffer.concat([buffer, chunk]);
    const match = /^(\d+):/.exec(buffer.toString('utf8'));
    const length = match ? Number(match[1]) : null;
    if (length === null || buffer.length < match[0].length + length) {
      if (buffer.length > maxBytes) finish(new Error(`the greeting exceeded ${maxBytes} bytes without a complete frame`));
      return;
    }
    const body = buffer.slice(match[0].length, match[0].length + length);
    try {
      const frame = JSON.parse(body.toString('utf8'));
      finish(null, frame);
    } catch {
      finish(new Error('the devtools greeting is not valid JSON'));
    }
  };
  const timer = setTimeout(
    () => finish(new Error(`timed out after ${timeoutMs}ms waiting for the devtools greeting`)), timeoutMs);
  socket.on('data', onData);
});

export function devtoolsScenarios({ exe, verbose, timeoutMs }) {
  return {
    devtools: async () => {
      const problems = [];
      const expect = (what, condition, detail = '') => {
        console.log(`    ${condition ? 'ok  ' : 'FAIL'} ${what}${condition ? '' : `: ${detail}`}`);
        if (!condition) problems.push(`${what}: ${detail}`);
      };
      // Stops a runtime and waits until nobody holds its log pipe; a hang is a problem of the scenario.
      const stopAndClose = async (running, what) => {
        running.stop();
        expect(`${what} lets go of its log pipe after the stop`, await running.waitForClosed(5000), 'the pipe stayed open for 5000 ms');
      };
      // The whole scenario runs on isolated profiles: nothing touches the user's real home.
      const dir = mkdtempSync(join(os.tmpdir(), 'alef-e2e-devtools-'));
      const appHome = join(dir, 'app-home');
      const isolation = () => ({
        ALEF_APP_HOME: appHome,
        ALEF_HOME: join(dir, 'home'),
        ...(process.platform === 'win32' ? { APPDATA: join(dir, 'appdata'), LOCALAPPDATA: join(dir, 'appdata') } : {}),
        ...(process.platform === 'linux' ? { XDG_DATA_HOME: join(dir, 'xdg', 'data'), XDG_CONFIG_HOME: join(dir, 'xdg', 'config'), XDG_CACHE_HOME: join(dir, 'xdg', 'cache') } : {}),
      });
      try {
        const site = prepareSite('devtools', 'dev');
        const port = await freePort();
        // Sub-check A: the flag starts the server, the stderr line carries port and token, and a
        // correct token gets the length-prefixed greeting from root. servo-devtools 0.6.0 compares
        // the first 28 bytes of the stream with a packet whose length prefix is the constant 25,
        // which is right only for an 8-digit token; its token is the hex of a random u32, so one
        // run in sixteen cannot be authenticated by anybody. Such a run is not a failure of the
        // runtime: a token of another length is restarted (a new random token), and the
        // scenario fails if no 8-digit token comes in six starts.
        let served = false;
        for (let attempt = 1; attempt <= 6 && !served; attempt++) {
          const flagged = startApp({ exe, args: ['--app', site, `--devtools=${port}`], verbose, env: isolation() });
          try {
            const line = await flagged.waitFor(
              candidate => /^ALEF_E2E devtools port=(\d+) token=(\S+)$/.test(candidate),
              timeoutMs, 'the devtools stderr line');
            const match = /^ALEF_E2E devtools port=(\d+) token=(\S+)$/.exec(line);
            expect('the devtools server reports the requested port', match[1] === String(port), line);
            if (match[2].length !== 8) {
              console.log(`    devtools: token ${match[2]} is not 8 digits (upstream cannot authenticate it), restarting`);
              continue;
            }
            const socket = await withTimeout(connect(port), 10000, 'the devtools connection');
            try {
              // Auth first: the packet goes out before the greeting is awaited.
              socket.write(authPacket(match[2]));
              const frame = await readFrame(socket, { timeoutMs: 10000, maxBytes: 1024 * 1024 });
              expect('the greeting is a length-prefixed JSON frame from root', frame !== null && frame.from === 'root', JSON.stringify(frame));
            } finally {
              socket.destroy();
            }
            try {
              const wrong = await withTimeout(connect(port), 10000, 'the wrong-token connection');
              wrong.write(authPacket('DEADBEEF'));
              expect('the server closes a connection with a wrong token', await closedWithin(wrong, 10000), 'the socket stayed open');
            } catch (error) {
              expect('the server closes a connection with a wrong token', false, error.message);
            }
            served = true;
          } catch (error) {
            expect('the flagged runtime serves the devtools protocol', false, error.message);
            break;
          } finally {
            await stopAndClose(flagged, 'the flagged runtime');
          }
        }
        if (!served && problems.length === 0) {
          expect('an 8-digit devtools token came within six starts', false, 'six runs gave a shorter token');
        }


        // Sub-check C: without the flag nothing of ours listens on the default port 7000 (a machine
        // where something else already does is not a failure of the runtime: the check is skipped).
        const occupied = await withTimeout(new Promise(resolvePromise => {
          const probe = net.connect(7000, '127.0.0.1');
          probe.once('error', () => resolvePromise(false));
          probe.once('connect', () => { probe.destroy(); resolvePromise(true); });
        }), 10000, 'the probe of port 7000');
        if (occupied) console.log('    devtools: port 7000 is taken by another program, the default-port check is skipped');
        const plain = startApp({ exe, args: ['--app', site], verbose, env: isolation() });
        try {
          await plain.waitFor(line => line.includes('ALEF_READY'), timeoutMs, 'ALEF_READY');
          const outcome = await withTimeout(new Promise((resolvePromise, reject) => {
            const socket = net.connect(7000, '127.0.0.1');
            socket.once('error', error => resolvePromise(error.code ?? String(error)));
            socket.once('connect', () => {
              socket.destroy();
              reject(new Error('something listens on port 7000 although --devtools was not given'));
            });
          }), 10000, 'the connection to port 7000 to be refused');
          if (!occupied) expect('the default devtools port 7000 is closed without the flag', outcome === 'ECONNREFUSED', String(outcome));
        } catch (error) {
          if (!occupied) expect('the default devtools port 7000 is closed without the flag', false, error.message);
        } finally {
          await stopAndClose(plain, 'the runtime without the flag');
        }

        // Sub-check D: the port already taken by another listener is refused with exit 1.
        const busyPort = await freePort();
        const holder = await holdPort(busyPort);
        const busy = startApp({ exe, args: ['--app', site, `--devtools=${busyPort}`], verbose, env: isolation() });
        try {
          const exit = await busy.waitForExit(timeoutMs);
          expect('the runtime refuses a busy devtools port with exit 1', exit?.code === 1, `exit ${exit?.code}`);
          expect('the busy-port message names the port', busy.stderr().toString('utf8')
            .includes(`DEVTOOLS_UNAVAILABLE: port ${busyPort} is not available`), busy.stderr().toString('utf8').slice(-300));
        } catch (error) {
          expect('the runtime refuses a busy devtools port with exit 1', false, error.message);
        } finally {
          await stopAndClose(busy, 'the runtime that met a busy port');
          await holder.close();
        }

        // Sub-check E: a packaged application refuses `--devtools` with exit 2, and runs without it.
        const packageSite = prepareSite('devtools-package', 'dev');
        mkdirSync(scratch, { recursive: true });
        const pkgPath = join(scratch, 'devtools.alef');
        try {
          await pack(packageSite, { unsigned: true, out: pkgPath });
        } catch (error) {
          expect('the tools CLI packs the unsigned package', false, error?.message ?? String(error));
        }
        {
          const refuse = startApp({ exe, args: ['run', pkgPath, '--devtools'], verbose, env: isolation() });
          try {
            const exit = await refuse.waitForExit(timeoutMs);
            expect('alef run refuses --devtools with exit 2', exit?.code === 2, `exit ${exit?.code}`);
            expect('the package refusal mentions --devtools', refuse.stderr().toString('utf8').includes('--devtools'),
              refuse.stderr().toString('utf8').slice(-300));
          } catch (error) {
            expect('alef run refuses --devtools with exit 2', false, error.message);
          } finally {
            await stopAndClose(refuse, 'the refused package run');
          }
          const packaged = startApp({ exe, args: ['run', pkgPath, '--allow-unsigned'], verbose, env: isolation() });
          try {
            await packaged.waitFor(line => line.includes('ALEF_READY'), timeoutMs, 'ALEF_READY of the packaged run');
            expect('the unsigned package runs without --devtools', true);
          } catch (error) {
            expect('the unsigned package runs without --devtools', false, error.message);
          } finally {
            await stopAndClose(packaged, 'the package run');
          }
        }
        return { problems, lines: [] };
      } finally {
        rmSync(dir, { recursive: true, force: true });
      }
    },
  };
}
