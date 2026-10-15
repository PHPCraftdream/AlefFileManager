// SPDX-License-Identifier: MIT OR Apache-2.0
// Signed package updater E2E: real packages, loopback feed, and sandboxed runtime homes.
import { spawnSync } from 'node:child_process';
import { createHash } from 'node:crypto';
import { cpSync, copyFileSync, existsSync, mkdirSync, mkdtempSync, readdirSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import os from 'node:os';
import { join } from 'node:path';
import { here, listen, startApp, transpileApi } from '../../lib.mjs';
import { createKey, pack } from '../../../../packages/tools/src/index.mjs';

const ID = 'org.alef.e2e.updater';

function manifest(version, endpoint, publicKey, id = ID, scope = endpoint) {
  return `id: ${id}\nname: Alef updater E2E\nversion:: ${version}\nwindows: [ { label: main, url: /index.html, width: 640, height: 480 } ]\nexternal: { connect: [], load: { scripts: [], styles: [], images: [], fonts: [], media: [], frames: [] } }\npermissions: { fs: { read: [], write: [$TEMP/**] }, cli: { exec: [] }, net: { http: [${scope}], socket: [] }, shell: { openExternal: [] }, clipboard: { read: false }, shortcut: { global: false }, secrets: false, app: { env: [] } }\nupdater: { endpoint: ${endpoint}, publicKey: ${publicKey} }\n`;
}

const PAGE = `import { updater } from './src/index.js';
import { api, report } from './harness.js';

const outcome = await (await fetch('targets.json')).json();
try {
  const checked = await updater.check();
  if (outcome.mode === 'success') {
    if (!checked || checked.available !== true || checked.version !== '2.0.0') throw new Error('update check did not offer 2.0.0: ' + JSON.stringify(checked));
    let progress = false;
    await updater.download({ onProgress: p => { if (p.received > 0) progress = true; } });
    if (!progress) throw new Error('download progress was not reported');
    await report('UPDATER_DOWNLOAD_OK');
    await api.app.exit(0);
  } else {
    await updater.download();
    throw new Error('unexpected successful download');
  }
} catch (error) {
  const code = error?.code ?? error?.message ?? String(error);
  if (outcome.mode === 'success') { console.error('updater E2E: ' + code); await api.app.exit(10); }
  if (code !== outcome.expected) { console.error('updater E2E: expected ' + outcome.expected + ', got ' + code); await api.app.exit(10); }
  await report('UPDATER_ERROR=' + code);
  await api.app.exit(0);
}`;

const UPDATE_PAGE = `import { updater } from './src/index.js';
import { api, report } from './harness.js';

const info = await api.app.info();
if (info.version !== '1.0.0') {
  const first = await api.app.requestSingleInstance();
  if (first === true) {
    await report('UPDATER_UPDATED=' + info.version);
    await api.app.exit(0);
  } else {
    await report('UPDATER_SECOND_INSTANCE');
    await api.app.exit(10);
  }
} else {
  try {
    const checked = await updater.check();
    if (!checked || checked.available !== true || checked.version !== '2.0.0') throw new Error('no offer: ' + JSON.stringify(checked));
    await updater.download();
    await report('UPDATER_INSTALLING');
    await updater.install();
    // The promise never resolves on success; reaching this line means the runtime failed to replace us.
    await report('UPDATER_INSTALL_RESOLVED');
    await api.app.exit(10);
  } catch (error) {
    await report('UPDATER_INSTALL_ERROR=' + (error?.code ?? error?.message ?? String(error)));
    await api.app.exit(10);
  }
}`;

function appManifest(version, endpoint, publicKey, id = ID, scope = endpoint) {
  return manifest(version, endpoint, publicKey, id, scope).replace('windows: [ { label: main, url: /index.html, width: 640, height: 480 } ]', `windows: [\n { label: main, url: /index.html, width: 640, height: 480 }\n ]`);
}

export function updaterScenarios({ exe, verbose, timeoutMs }) {
  return {
    updater: async () => {
      const problems = [];
      const dir = mkdtempSync(join(os.tmpdir(), `alef-e2e-updater-${process.pid}-`));
      const keyHome = join(dir, 'keys');
      const xdg = Object.fromEntries(['DATA', 'CONFIG', 'CACHE', 'STATE', 'RUNTIME'].map(name => [`XDG_${name}_HOME`, join(dir, 'xdg', name.toLowerCase())]));
      let server;
      let running;
      let reportedPids = [];
      let replacementExited = false;
      try {
        const key = await createKey(`update-${process.pid}`, { home: keyHome });
        const wrongKey = await createKey(`wrong-${process.pid}`, { home: join(dir, 'wrong-keys') });
        const api = transpileApi();
        const packages = new Map();
        const makeCandidate = async (name, appId = ID, signingKey = `update-${process.pid}`) => {
          const base = join(dir, name);
          mkdirSync(join(base, 'dist'), { recursive: true });
          const publicKey = Buffer.from(name === 'wrong-key' ? wrongKey.key : key.key).toString('base64');
          writeFileSync(join(base, 'alef.ktav'), appManifest('2.0.0', server.origin, publicKey, appId, `${server.origin}/*`));
          writeFileSync(join(base, 'dist', 'index.html'), '<!doctype html><html><head><meta charset="utf-8"><title>candidate</title><script type="module" src="page.js"></script></head><body></body></html>');
          writeFileSync(join(base, 'dist', 'page.js'), UPDATE_PAGE);
          copyFileSync(join(here, 'harness.js'), join(base, 'dist', 'harness.js'));
          for (const folder of ['src', 'types']) cpSync(join(api, folder), join(base, 'dist', folder), { recursive: true });
          const out = join(dir, `${name}.alef`);
          await pack(base, { home: signingKey.startsWith('wrong-') ? join(dir, 'wrong-keys') : keyHome, key: signingKey, out });
          const data = readFileSync(out); packages.set(`/${name}.alef`, data);
          return out;
        };
        server = await listen((request, response) => {
          if (request.url.startsWith('/feed')) {
            const mode = new URL(request.url, server.origin).searchParams.get('mode');
            const filename = mode === 'id' ? 'wrong-id' : mode === 'key' ? 'wrong-key' : 'good';
            const digest = createHash('sha256').update(packages.get(`/${filename}.alef`)).digest('hex');
            response.writeHead(200, { 'content-type': 'application/json' });
            response.end(JSON.stringify({ version: '2.0.0', package: `/${filename}.alef`, sha256: digest }));
          } else if (packages.has(request.url)) response.writeHead(200, { 'content-length': packages.get(request.url).length }).end(packages.get(request.url));
          else response.writeHead(404).end();
        });
        const makePackage = async name => {
          const base = join(dir, `run-${name}`); mkdirSync(join(base, 'dist'), { recursive: true });
          const endpoint = `${server.origin}/feed?mode=${name}`;
          writeFileSync(join(base, 'alef.ktav'), appManifest('1.0.0', endpoint, Buffer.from(key.key).toString('base64'), ID, `${server.origin}/*`));
          copyFileSync(join(here, 'harness.js'), join(base, 'dist', 'harness.js'));
          writeFileSync(join(base, 'dist', 'index.html'), '<!doctype html><html><head><meta charset="utf-8"><title>updater</title><script type="module" src="page.js"></script></head><body></body></html>');
          writeFileSync(join(base, 'dist', 'page.js'), PAGE);
          writeFileSync(join(base, 'dist', 'targets.json'), JSON.stringify({ mode: name === 'success' ? name : 'negative', expected: name === 'id' ? 'UPDATE_ID_MISMATCH' : 'UPDATE_KEY_MISMATCH' }));
          for (const folder of ['src', 'types']) cpSync(join(api, folder), join(base, 'dist', folder), { recursive: true });
          const pkg = join(dir, `running-${name}.alef`);
          await pack(base, { home: keyHome, key: `update-${process.pid}`, out: pkg });
          return pkg;
        };
        await makeCandidate('good', ID);
        await makeCandidate('wrong-id', `${ID}.wrong`);
        await makeCandidate('wrong-key', ID, `wrong-${process.pid}`);
        const cases = [
          { name: 'success', pkg: await makePackage('success') },
          { name: 'id', pkg: await makePackage('id'), expected: 'UPDATE_ID_MISMATCH' },
          { name: 'key', pkg: await makePackage('key'), expected: 'UPDATE_KEY_MISMATCH' },
        ];
        for (const item of cases) {
          const home = join(dir, `home-${item.name}`); mkdirSync(home, { recursive: true });
          running = startApp({ exe, verbose, args: ['run', item.pkg, '--trust-key', key.fingerprint], env: { ALEF_HOME: home, ALEF_APP_HOME: home, ALEF_E2E_CONSENT: '*=allow', ...xdg } });
          try {
            const exit = await running.waitForExit(timeoutMs);
            const log = running.lines.join('\n');
            if (item.expected) {
              if (exit?.code !== 0 || !log.includes(`UPDATER_ERROR=${item.expected}`)) problems.push(`${item.name}: expected ${item.expected}; exit=${exit?.code}; ${log}`);
            } else if (exit?.code !== 0 || !running.lines.some(line => line.includes('UPDATER_DOWNLOAD_OK'))) problems.push(`success: runtime exit ${exit?.code}; ${log}`);
            const packageRoot = join(home, 'data', 'alef-packages');
            const walk = (path, depth = 0) => depth > 4 || !existsSync(path) ? [] : readdirSync(path, { withFileTypes: true }).flatMap(entry => entry.isDirectory() ? walk(join(path, entry.name), depth + 1) : [join(path, entry.name)]);
            const files = walk(packageRoot);
            const updateFiles = files.filter(path => path.includes(`${join('updates', '2.0.0.alef')}`));
            if (item.expected) {
              const leftovers = files.filter(path => path.includes('updates') && !path.endsWith('.alef-update.lock'));
              if (leftovers.length > 0) problems.push(`${item.name}: rejected package left update artifacts: ${leftovers}`);
            } else {
              const expectedFile = files.find(path => path.endsWith(join('updates', '2.0.0.alef')));
              if (!expectedFile || updateFiles.length !== 1) problems.push('success: expected installed 2.0.0.alef');
              else {
                const entries = readdirSync(join(expectedFile, '..'));
                if (entries.some(name => name !== '2.0.0.alef' && name !== '.alef-update.lock')) problems.push(`success: unexpected update artifacts: ${entries}`);
              }
            }
          } finally {
            if (running && !running.exit) running.stop();
            await running?.waitForExit(5000);
            await running?.waitForClosed(5000);
            running = undefined;
          }
        }
        // The full install-and-restart path: a running 1.0.0 downloads 2.0.0, offers it, installs
        // it and is replaced by a detached 2.0.0 process; the install promise never resolves.
        // The replacement proves the single-instance handover via app.requestSingleInstance.
        const updateBase = join(dir, 'update-base');
        mkdirSync(join(updateBase, 'dist'), { recursive: true });
        writeFileSync(join(updateBase, 'alef.ktav'), appManifest('1.0.0', `${server.origin}/feed`, Buffer.from(key.key).toString('base64')));
        copyFileSync(join(here, 'harness.js'), join(updateBase, 'dist', 'harness.js'));
        writeFileSync(join(updateBase, 'dist', 'index.html'), '<!doctype html><html><head><meta charset="utf-8"><title>updater install</title><script type="module" src="page.js"></script></head><body></body></html>');
        writeFileSync(join(updateBase, 'dist', 'page.js'), UPDATE_PAGE);
        for (const folder of ['src', 'types']) cpSync(join(api, folder), join(updateBase, 'dist', folder), { recursive: true });
        const updatePkg = join(dir, 'update-1.0.0.alef');
        await pack(updateBase, { home: keyHome, key: `update-${process.pid}`, out: updatePkg });
        const installHome = join(dir, 'home-update');
        mkdirSync(installHome, { recursive: true });
        const installed = spawnSync(exe, ['install', updatePkg, '--trust-key', key.fingerprint], {
          env: { ...process.env, ALEF_E2E: '1', ALEF_HOME: installHome, ALEF_APPLICATIONS_DIR: join(dir, 'links-update'), ...xdg },
          encoding: 'utf8',
          timeout: timeoutMs,
        });
        if (installed.status !== 0) problems.push(`alef install failed: ${installed.status} ${installed.stderr}`);
        const storeDir = join(installHome, 'apps', createHash('sha256').update(ID).digest('hex'));
        const storePath = join(storeDir, '1.0.0.alef');
        if (!existsSync(storePath)) problems.push('the store has no 1.0.0.alef after alef install');
        // Named, bounded at-most allowance for the CURRENT §3.2/§5 violation: updater install
        // prepares the storage synchronously (updater install Storage::prepare) // audit #160
        running = startApp({ exe, verbose, args: ['run', storePath], expectedBusStalls: [{ kind: 'call/updater.install', minMs: 300, maxMs: 800, count: 1 }], env: { ALEF_HOME: installHome, ALEF_APP_HOME: installHome, ALEF_E2E_CONSENT: '*=allow', ALEF_APPLICATIONS_DIR: join(dir, 'links-update'), ...xdg } });
        try {
          await running.waitFor(line => line.includes('UPDATER_INSTALLING'), timeoutMs, 'the old version to offer and download the update');
          const exit = await running.waitForExit(timeoutMs);
          if (exit?.code !== 0) problems.push(`the old version exited with ${exit ? exit.code : 'no exit'}`);
          // Record every runtime pid as soon as it appears, so a failing wait still leaves the
          // detached replacement recorded for the failure cleanup below.
          const readyDeadline = Date.now() + timeoutMs;
          while (running.lines.filter(line => line.startsWith('ALEF_READY')).length < 2) {
            if (running.exit) break;
            if (Date.now() > readyDeadline) throw new Error('the replacement process never became ready');
            await new Promise(resolve => setTimeout(resolve, 50));
          }
          reportedPids = running.lines
            .filter(line => line.startsWith('ALEF_READY'))
            .map(line => /pid=(\d+)/.exec(line)?.[1])
            .filter(pid => pid !== undefined && /^\d+$/.test(pid))
            .map(Number);
          await running.waitFor(line => line.includes('UPDATER_UPDATED=2.0.0'), timeoutMs, 'the new version to report itself', { survivesExit: true });
          if (running.lines.some(line => line.includes('UPDATER_INSTALL_RESOLVED'))) problems.push('updater.install resolved in the old process');
          const failed = running.lines.find(line => line.includes('UPDATER_INSTALL_ERROR='));
          if (failed) problems.push(`updater.install failed: ${failed}`);
          if (running.lines.some(line => line.includes('UPDATER_SECOND_INSTANCE'))) problems.push('the replacement was told it is a second instance');
          const pids = running.lines.filter(line => line.startsWith('ALEF_READY')).map(line => /pid=(\d+)/.exec(line)?.[1]);
          if (pids.length < 2 || pids[0] === pids[1]) problems.push(`expected a replacement process, saw ${JSON.stringify(pids)}`);
          const stored = existsSync(storeDir) ? readdirSync(storeDir).sort() : [];
          if (!stored.includes('2.0.0.alef') || stored.includes('1.0.0.alef')) problems.push(`store after update: ${stored.join(', ')}`);
          if (await running.waitForClosed(30000)) replacementExited = true;
          else problems.push('the new version did not exit');
        } finally {
          if (running && !running.exit) running.stop();
          await running?.waitForExit(5000);
          await running?.waitForClosed(5000);
          running = undefined;
        }
      } catch (error) { problems.push(error.stack ?? error.message); }
      finally {
        if (running) {
          if (!running.exit) running.stop();
          await running.waitForExit(5000);
          await running.waitForClosed(5000);
        }
        if (!replacementExited) {
          for (const pid of reportedPids) {
            if (Number.isFinite(pid)) { try { process.kill(pid); } catch { /* already gone */ } }
          }
        }
        await server?.close();
        rmSync(dir, { recursive: true, force: true });
      }
      return { problems, lines: [] };
    },
  };
}
