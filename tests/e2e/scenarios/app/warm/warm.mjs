// SPDX-License-Identifier: MIT OR Apache-2.0
// The warm-start scenario (docs/stages/m11-warm-start.md, task M11.11): the broker of this session,
// the windowed spare it keeps, the handoff of a real application to the spare, the decline of a
// console application back to the cold path, and the paired cold/warm timings of the observable
// `ALEF_READY` moment. The windowless slot is disabled by design; it is never awaited.
// The store identity of the session is the exact ALEF_HOME/ALEF_APP_HOME values: the scenario
// builds the env fragment once and passes the SAME fragment to the broker (through `alef warm
// start`) and to every `alef run` client, otherwise the handshake rejects (StoreMismatch).
import { spawn, spawnSync } from 'node:child_process';
import { mkdirSync, mkdtempSync, readdirSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import { join } from 'node:path';

import { prepareSite, startApp } from '../../../lib.mjs';
import { percentile } from '../../startup.mjs';

/** How many paired runs the summary measures (cold first, then the same app warm). */
export const RUNS_WARM = 5;

/** The status line of `alef warm status --json`, or null when it is not the expected object. */
export function parseStatusJson(text) {
  try {
    const parsed = JSON.parse(text);
    if (parsed === null || typeof parsed !== 'object' || Array.isArray(parsed) || !('broker' in parsed)) return null;
    return parsed;
  } catch {
    return null;
  }
}

/** The state string of the slot of `kind` ('windowed'|'windowless'), or undefined. */
export function slotState(status, kind) {
  return status?.broker?.slots?.find(slot => slot.kind === kind)?.state;
}

/**
 * The exact env fragment every child of the session must share: the broker and the clients derive
 * the same endpoint only from identical ALEF_HOME/ALEF_APP_HOME. XDG_RUNTIME_DIR (the Unix base of
 * the socket path) is set only off Windows; Windows derives a pipe name from the session identity.
 */
export function warmEnvironment({ home, appHome, runtimeDir }, platform = process.platform) {
  return {
    ALEF_HOME: home,
    ALEF_APP_HOME: appHome,
    ...(platform !== 'win32' && runtimeDir !== undefined ? { XDG_RUNTIME_DIR: runtimeDir } : {}),
  };
}

/** Rows (mode, n, p50, p95, min, max) of two samples of the same observable measurement. */
export function modeTable(cold, warm) {
  const row = (mode, values) => ({
    mode,
    n: values.length,
    p50: percentile(values, 0.5),
    p95: percentile(values, 0.95),
    min: Math.min(...values),
    max: Math.max(...values),
  });
  return [row('cold', cold), row('warm', warm)];
}

const header = ['mode', 'n', 'p50', 'p95', 'min', 'max'];

function table(rows) {
  const cell = (row, column) => String(typeof row[column] === 'number' ? Number(row[column].toFixed(1)) : row[column]);
  const width = column => Math.max(...[header[column], ...rows.map(row => cell(row, header[column]))].map(text => text.length));
  const line = row => `| ${header.map((name, column) => cell(row, name).padEnd(width(column))).join(' | ')} |`;
  return [line(header), `| ${header.map((_, column) => '-'.repeat(width(column))).join(' | ')} |`, ...rows.map(line)].join('\n');
}

/** Markdown block for $GITHUB_STEP_SUMMARY in the shape of startup.mjs's summaryMarkdown. */
export function warmSummaryMarkdown(os, mode, values) {
  const row = modeTable(values, values)[0];
  return [`### warm — ${os} — ${mode} — N=${row.n}`, table([{ ...row }])].join('\n');
}

const ALEF_READY = 'ALEF_READY';
const WARM_PROBE = 'ALEF_E2E WARM-PROBE info-ok';
/** Marks only the warm spare path can print (a pure cold client never prints `assigned`). */
const SPARE_MARK = 'ALEF_TIMING assigned';
const CLIENT_MARK_BEGIN = 'ALEF_TIMING warm-client-begin';
const CLIENT_MARK_END = 'ALEF_TIMING warm-client-end';
/** The minimal page the warm run hands to the spare: one info call, one marker, a clean exit. */
const PROBE_PAGE = [
  "import { api } from './harness.js';",
  'const main = async () => {',
  '  await api.app.info();',
  "  console.log('WARM-PROBE info-ok');",
  "  api.call('e2e.report', { line: 'WARM-PROBE info-ok' }).catch(() => {});",
  '  await api.app.quit(0);',
  '};',
  'main();',
].join('\n');

export function warmScenarios({ exe, verbose, timeoutMs }) {
  return {
    // Everything runs under one scenario; every child is awaited with a deadline and cleaned up
    // in `finally` (the broker by its own `stop`, then by pid if it ignored that).
    async warm() {
      const problems = [];
      const lines = [];
      const envTemp = mkdtempSync(join(tmpdir(), 'alef-warm-env-'));
      const home = join(envTemp, 'home');
      const appHome = join(envTemp, 'app-home');
      mkdirSync(home, { recursive: true });
      mkdirSync(appHome, { recursive: true });
      // Short base: the socket path must fit the ~100 byte Unix limit; $TMPDIR is long on macOS, /tmp is not.
      const runtimeDir = process.platform === 'win32' ? undefined : mkdtempSync(join('/tmp', 'alef-warm-'));
      const fragment = warmEnvironment({ home, appHome, runtimeDir });
      const env = { ...process.env, ...fragment };
      let brokerPid = null;
      let basicSite = null;

      /** One `alef <args>` child of the session env; its lines are collected, its exit awaited with a deadline. */
      async function runCli(args, { envExtra = {}, deadlineMs = timeoutMs } = {}) {
        const child = spawn(exe, args, { env: { ...env, ...envExtra }, stdio: ['ignore', 'pipe', 'pipe'] });
        const out = [];
        const err = [];
        child.stdout.on('data', chunk => out.push(...String(chunk).split('\n')));
        child.stderr.on('data', chunk => err.push(...String(chunk).split('\n')));
        const exit = await new Promise(resolve => {
          const timer = setTimeout(() => {
            child.kill();
            resolve(null);
          }, deadlineMs);
          child.on('exit', code => {
            clearTimeout(timer);
            resolve({ code });
          });
        });
        return { exit, code: exit?.code, out, err, all: [...out, ...err].map(line => line.trim()).filter(Boolean) };
      }

      /** Polls `warm status --json` until `predicate` holds or the deadline passes; returns the last status. */
      async function pollStatus(predicate, what, deadlineMs) {
        const deadline = Date.now() + deadlineMs;
        let status = null;
        for (;;) {
          const run = spawnSync(exe, ['warm', 'status', '--json'], { encoding: 'utf8', timeout: 15000, env });
          status = parseStatusJson(run.stdout?.trim() ?? '');
          if (status !== null && predicate(status)) return { ok: true, status };
          if (Date.now() > deadline) return { ok: false, status };
          await new Promise(resolve => setTimeout(resolve, 250));
        }
      }

      async function grantedTotal(what) {
        const { ok, status } = await pollStatus(state => state.broker.running === true, what, 15000);
        if (!ok || typeof status.broker.granted_total !== 'number') throw new Error(`could not read granted_total (${what})`);
        return status.broker.granted_total;
      }

      /** The polite shutdown, with a pid kill as the bounded fallback; never a kill by name. */
      async function stopBroker() {
        await runCli(['warm', 'stop'], { deadlineMs: 30000 });
        const deadline = Date.now() + 15000;
        for (;;) {
          const { status } = await pollStatus(state => state.broker.running === false, 'the broker to stop', 100);
          if (status !== null && status.broker.running === false) break;
          if (Date.now() > deadline) {
            if (brokerPid !== null) {
              try { process.kill(brokerPid); } catch { /* already gone */ }
              await new Promise(resolve => setTimeout(resolve, 1000));
            }
            break;
          }
          await new Promise(resolve => setTimeout(resolve, 250));
        }
      }

      /** One `alef run <site>` (the warm-capable client); returns after exit with a deadline. */
      async function runClient(site, { envExtra = {}, input } = {}) {
        const started = Date.now();
        const running = startApp({ exe, args: ['run', site], env: { ...fragment, ...envExtra }, verbose, input });
        const ready = await running.waitFor(line => line.startsWith(ALEF_READY), timeoutMs, ALEF_READY, { survivesExit: true }).catch(() => null);
        const exit = await running.waitForExit(timeoutMs);
        // The wall-clock spawn -> first ALEF_READY line; null unless the run succeeded completely.
        const readyMs = ready === null || exit?.code !== 0 ? null : Date.now() - started;
        return { running, exit, lines: running.lines, readyMs };
      }

      try {
        // -- 1. no-broker status ---------------------------------------------------------------
        {
          const run = await runCli(['warm', 'status', '--json']);
          const status = parseStatusJson(run.out.join('\n'));
          const ok = run.code === 0 && status !== null && status.broker.running === false;
          console.log(`    ${ok ? 'ok  ' : 'FAIL'} status without a broker: exit ${run.code}, broker.running=${status?.broker?.running}`);
          if (!ok) problems.push(`status without a broker: exit ${run.code}, json ${run.out.join(' ').slice(0, 200)}`);
        }

        // -- 2. start the broker and wait for the windowed spare -------------------------------
        {
          const run = await runCli(['warm', 'start']);
          const ok = run.code === 0 && run.all.some(line => line.includes('started'));
          console.log(`    ${ok ? 'ok  ' : 'FAIL'} warm start: exit ${run.code}, said ${run.all.join(' | ').slice(0, 120)}`);
          if (!ok) {
            problems.push(`warm start: exit ${run.code}, output ${run.all.join(' | ').slice(0, 200)}`);
            // The detached broker has no stderr: show what a foreground one says (diagnostics only).
            const probe = await runCli(['broker', '--no-tray'], { deadlineMs: 8000 });
            console.log([`    broker in the foreground (exit ${probe.code}):`, ...probe.all.map(line => `      | ${line}`)].join('\n'));
          }
          const running = await pollStatus(state => state.broker.running === true && slotState(state, 'windowed') === 'ready', 'the broker and a ready windowed slot', 60000);
          if (running.ok) brokerPid = running.status.broker.pid;
          console.log(`    ${running.ok ? 'ok  ' : 'FAIL'} windowed slot ready (pid ${brokerPid}): ${JSON.stringify(running.status?.broker?.slots ?? null)}`);
          if (!running.ok) problems.push(`the windowed slot never became ready: ${JSON.stringify(running.status?.broker ?? null)}`);
        }

        // -- 3. a warm run of a windowed application -------------------------------------------
        let grantedBefore = 0;
        let warmRun = null;
        {
          basicSite = prepareSite('warm-basic', 'modules/app/basic');
          writeFileSync(join(basicSite, 'dist', 'page.js'), PROBE_PAGE);
          grantedBefore = await grantedTotal('before the warm run');
          warmRun = await runClient(basicSite, { envExtra: { ALEF_WARM_DEBUG: '1', ALEF_TIMING: '1' } });
          lines.push(...warmRun.lines);
          const grantedAfter = await grantedTotal('after the warm run');
          const marks = warmRun.lines;
          const spareEvidence = marks.some(line => line.includes(SPARE_MARK))
            || (marks.some(line => line.includes(CLIENT_MARK_BEGIN)) && marks.some(line => line.includes(CLIENT_MARK_END)));
          const checks = {
            'exit code 0': warmRun.exit?.code === 0,
            [ALEF_READY]: warmRun.lines.some(line => line.startsWith(ALEF_READY)),
            'exactly one probe marker': warmRun.lines.filter(line => line.includes(WARM_PROBE)).length === 1,
            'granted_total +1': grantedAfter === grantedBefore + 1,
            'spare-path timing marks': spareEvidence,
          };
          for (const [name, ok] of Object.entries(checks)) console.log(`    ${ok ? 'ok  ' : 'FAIL'} warm run: ${name}`);
          problems.push(...Object.entries(checks).filter(([, ok]) => !ok).map(([name]) => `warm run: ${name}`));
        }

        // -- 4. the spare is replaced while nobody runs ----------------------------------------
        {
          const before = await grantedTotal('before the replacement wait');
          const replacement = await pollStatus(state => slotState(state, 'windowed') === 'ready', 'the windowed slot to be ready again', 60000);
          const after = replacement.status?.broker?.granted_total;
          const ok = replacement.ok && after === before;
          console.log(`    ${ok ? 'ok  ' : 'FAIL'} spare replaced while idle: slot=${slotState(replacement.status ?? {}, 'windowed')}, granted ${before} -> ${after}`);
          if (!ok) problems.push(`the pool did not recover a ready windowed spare, or granted_total moved while idle (${before} -> ${after})`);
        }

        // -- 5. a console application declines to the cold path, exactly once -------------------
        {
          const size = 64;
          const input = Buffer.alloc(size);
          for (let at = 0; at < size; at += 1) input[at] = (at * 31 + (at >> 8)) & 255;
          const consoleSite = prepareSite('warm-console', 'modules/app/console', { targets: { size } });
          const cold = await runClient(consoleSite, { envExtra: { ALEF_WARM: '0' }, input });
          const coldResult = cold.lines.filter(line => line.startsWith('ALEF_E2E RESULT'));
          const coldConsole = cold.lines.filter(line => line.includes('ALEF_CONSOLE')).length;
          const warm = await runClient(consoleSite, { envExtra: { ALEF_WARM_DEBUG: '1' }, input });
          const warmResult = warm.lines.filter(line => line.startsWith('ALEF_E2E RESULT'));
          const warmConsole = warm.lines.filter(line => line.includes('ALEF_CONSOLE')).length;
          const granted = await grantedTotal('after the console decline');
          const declined = warm.lines.some(line => line.includes('warm start unavailable'));
          const pool = await pollStatus(state => slotState(state, 'windowed') === 'ready', 'the pool after the console decline', 60000);
          const checks = {
            'the cold baseline exits like the page says (7)': cold.exit?.code === 7,
            'the warm attempt exits identically': warm.exit?.code === cold.exit?.code,
            'the cold baseline prints one RESULT': coldResult.length === 1,
            'the warm attempt prints the identical RESULT once': warmResult.length === 1 && warmResult[0] === coldResult[0],
            'the application ran exactly once (stable marker count)': coldConsole === 1 && warmConsole === coldConsole,
            'the decline is proven on stderr': declined,
            'the broker stays up': granted === grantedBefore + 1,
            'the pool recovered': pool.ok,
          };
          for (const [name, ok] of Object.entries(checks)) console.log(`    ${ok ? 'ok  ' : 'FAIL'} console decline: ${name}`);
          problems.push(...Object.entries(checks).filter(([, ok]) => !ok).map(([name]) => `console decline: ${name}`));
          lines.push(...warm.lines);
        }

        // -- 6. ALEF_WARM=0 bypasses the broker ------------------------------------------------
        {
          const before = await grantedTotal('before the ALEF_WARM=0 run');
          const cold = await runClient(basicSite, { envExtra: { ALEF_WARM: '0', ALEF_WARM_DEBUG: '1', ALEF_TIMING: '1' } });
          const after = await grantedTotal('after the ALEF_WARM=0 run');
          const checks = {
            'exit code 0': cold.exit?.code === 0,
            [ALEF_READY]: cold.lines.some(line => line.startsWith(ALEF_READY)),
            'exactly one probe marker': cold.lines.filter(line => line.includes(WARM_PROBE)).length === 1,
            'no spare path marks': !cold.lines.some(line => line.includes(SPARE_MARK)),
            'granted_total unchanged': after === before,
          };
          for (const [name, ok] of Object.entries(checks)) console.log(`    ${ok ? 'ok  ' : 'FAIL'} ALEF_WARM=0: ${name}`);
          problems.push(...Object.entries(checks).filter(([, ok]) => !ok).map(([name]) => `ALEF_WARM=0: ${name}`));
          lines.push(...cold.lines);
        }

        // -- 7. stop ---------------------------------------------------------------------------
        {
          await stopBroker();
          const gone = await pollStatus(state => state.broker.running === false, 'the broker to report stopped', 15000);
          let pidGone = true;
          if (brokerPid !== null) {
            try { process.kill(brokerPid, 0); pidGone = false; } catch { pidGone = true; }
          }
          const again = await runCli(['warm', 'stop']);
          let socketGone = true;
          if (process.platform !== 'win32' && runtimeDir !== undefined) {
            const walk = directory => {
              for (const entry of readdirSync(directory, { withFileTypes: true })) {
                const path = join(directory, entry.name);
                if (entry.isDirectory()) walk(path);
                else if (entry.name === 'broker.sock') socketGone = false;
              }
            };
            walk(runtimeDir);
          }
          const checks = {
            'status says not running': gone.ok,
            [`the pid ${brokerPid} no longer exists`]: pidGone,
            ...(process.platform === 'win32' ? {} : { 'no broker.sock under XDG_RUNTIME_DIR': socketGone }),
            'the second stop exits 0 and says not running': again.code === 0 && again.all.some(line => line.includes('not running')),
          };
          for (const [name, ok] of Object.entries(checks)) console.log(`    ${ok ? 'ok  ' : 'FAIL'} stop: ${name}`);
          problems.push(...Object.entries(checks).filter(([, ok]) => !ok).map(([name]) => `stop: ${name}`));
        }

        // -- 8. paired cold/warm timings of the ALEF_READY moment ------------------------------
        {
          const measure = async envExtra => {
            const started = Date.now();
            const running = startApp({ exe, args: ['run', basicSite], env: { ...fragment, ...envExtra }, verbose });
            const ready = await running.waitFor(line => line.startsWith(ALEF_READY), timeoutMs, ALEF_READY, { survivesExit: true }).catch(() => null);
            const exit = await running.waitForExit(timeoutMs);
            if (ready === null || exit?.code !== 0) return null;
            return Date.now() - started;
          };
          const cold = [];
          for (let index = 0; index < RUNS_WARM; index += 1) {
            const ms = await measure({ ALEF_WARM: '0' });
            if (ms !== null) cold.push(ms);
          }
          await runCli(['warm', 'start']);
          const readySlot = await pollStatus(state => state.broker.running === true && slotState(state, 'windowed') === 'ready', 'the broker for the warm timings', 60000);
          if (readySlot.ok) brokerPid = readySlot.status.broker.pid;
          const warm = [];
          if (readySlot.ok) {
            for (let index = 0; index < RUNS_WARM; index += 1) {
              const ms = await measure({ ALEF_TIMING: '1' });
              if (ms !== null) warm.push(ms);
            }
          }
          const os = `${process.platform}/${process.arch}`;
          console.log(`    warm timings (spawn -> first ${ALEF_READY} line, ms; no threshold, CI machines are noisy):\n${table(modeTable(cold, warm))
            .split('\n').map(row => `      ${row}`).join('\n')}`);
          const complete = cold.length === RUNS_WARM && warm.length === RUNS_WARM;
          console.log(`    ${complete ? 'ok  ' : 'FAIL'} ${RUNS_WARM} successful cold and warm measurements (cold ${cold.length}, warm ${warm.length})`);
          if (!complete) problems.push(`the timing sample is incomplete: cold ${cold.length}/${RUNS_WARM}, warm ${warm.length}/${RUNS_WARM}`);
          const summaryFile = process.env.GITHUB_STEP_SUMMARY;
          if (summaryFile !== undefined) {
            try {
              writeFileSync(summaryFile, [
                warmSummaryMarkdown(os, 'cold', cold),
                warmSummaryMarkdown(os, 'warm', warm),
              ].join('\n\n') + '\n', { flag: 'a' });
            } catch (error) {
              problems.push(`could not write GITHUB_STEP_SUMMARY: ${error?.message ?? error}`);
            }
          }
          await stopBroker();
        }
      } finally {
        await stopBroker();
        // A just-stopped process may still hold its files for a moment (Windows): retry, never fail on it.
        for (const directory of [runtimeDir, envTemp]) {
          if (directory === undefined) continue;
          try { rmSync(directory, { recursive: true, force: true, maxRetries: 10, retryDelay: 300 }); }
          catch (error) { console.log(`    cleanup of ${directory} left files: ${error?.code ?? error}`); }
        }
      }
      return { problems, lines };
    },
  };
}
