// SPDX-License-Identifier: MIT OR Apache-2.0
// What the user sees while the application starts (Windows: the probe watches the windows of the
// process): no helper window flashes, and the main window is shown with content, not as an
// unpainted rectangle (white, with a black strip where it grew to its size).
import { spawnSync } from 'node:child_process';
import { rmSync } from 'node:fs';
import { join } from 'node:path';

import { here, prepareSite, quiet, scratch, startApp } from '../lib.mjs';

export function startupScenarios({ exe }) {
  return {
    async startup() {
      if (process.platform !== 'win32') {
        console.log('    skipped: the probe watches Win32 windows');
        return { problems: [], lines: [] };
      }
      if (quiet) {
        console.log('    skipped: it watches real windows, which appear on screen; ALEF_E2E_VISIBLE=1 runs it');
        return { problems: [], lines: [] };
      }
      const site = prepareSite('startup', 'startup');
      const pictures = join(scratch, 'startup-pictures');
      const run = spawnSync('powershell', [
        '-NoProfile', '-ExecutionPolicy', 'Bypass', '-File', join(here, 'apps', 'startup', 'startup-probe.ps1'),
        '-Exe', exe, '-Arguments', `--app "${site}"`, '-OutDir', pictures, '-Seconds', '60', '-Assert',
      ], { encoding: 'utf8', timeout: 150000 });
      const output = `${run.stdout ?? ''}${run.stderr ?? ''}`;
      const failures = output.split(/\r?\n/).filter(line => line.startsWith('ASSERT FAIL')).map(line => line.replace('ASSERT FAIL: ', ''));
      const problems = [...failures];
      if (failures.length === 0 && !output.includes('ASSERT OK')) problems.push(`the probe did not finish: ${output.slice(-400)}`);
      if (problems.length > 0) console.log(output.split(/\r?\n/).filter(line => /\b(show|create)\b|picture|ASSERT/.test(line)).map(line => `    | ${line}`).join('\n'));
      return { problems, lines: [] };
    },
  };
}

const BUS_XFAIL = 'XFAIL audit #160: the pre-launch consent gate correctly holds the probes until release, but the launcher\'s consent settle path is not yet proven off the BUS';
export function busScenarios({ drive, exe, verbose, timeoutMs }) {
  return {
    async bus() {
      const outcome = await drive({
        name: 'bus', app: 'modules/system/bus', expectedChecks: [
          'bus-warm-api-calls', 'bus-executor-entered', 'bus-unrelated-calls-while-blocked',
          'bus-event-roundtrip-while-blocked', 'bus-capacity-one-is-busy', 'bus-blocking-duration-bounded',
          'bus-invalid-duration-rejected', 'bus-negative-control-stalls',
        ],
        expectedBusStalls: [{ kind: 'call/e2e.blockBus', minMs: 150, maxMs: 5000, count: 1, required: true, after: 'ALEF_E2E bus-negative-begin', before: 'ALEF_E2E bus-negative-end' }],
      });
      const home = join(scratch, 'bus-consent-home');
      rmSync(home, { recursive: true, force: true });
      const site = prepareSite('bus-consent', 'modules/system/bus', { targets: { consentProbe: true } });
      let running;
      const lines = [...outcome.lines];
      const problems = [...outcome.problems];
      try {
        running = startApp({ exe, args: ['--app', site], verbose, undecidedConsent: true, env: {
          ALEF_HOME: home, ALEF_E2E_CONSENT_HOLD_MS: '3000',
          ALEF_E2E_CONSENT_UI: JSON.stringify([{ all: 'allow' }, { submit: true }]),
        } });
        const pending = await running.waitFor(line => line === 'ALEF_E2E consent-pending', timeoutMs, 'real consent-pending marker');
        const pendingAt = running.lines.indexOf(pending);
        const pendingStarted = performance.now();
        let premature = false;
        try { await running.waitFor(line => line.startsWith('ALEF_E2E bus-consent-probe-complete'), 300, 'probe completion before release'); premature = true; }
        catch (error) { if (!error.message.startsWith('timed out waiting')) throw error; }
        const releasedBeforeTimeout = running.lines.some(line => line.startsWith('ALEF_E2E consent-released'));
        const readyBeforeTimeout = running.lines.some(line => line.startsWith('ALEF_READY'));
        if (premature) problems.push('application probe completed while consent was pending');
        if (releasedBeforeTimeout) problems.push('consent was released before the bounded host probe attempt timed out');
        if (readyBeforeTimeout) problems.push('application became ALEF_READY while consent hold was active');
        if (pendingAt < 0) problems.push('consent-pending marker missing');
        lines.push('host attempted actual app probe for 300ms after pre-launch consent-pending');
        await running.waitFor(line => line.startsWith('ALEF_E2E consent-released'), 8000, 'consent release');
        const complete = await running.waitFor(line => line.startsWith('ALEF_E2E bus-consent-probe-complete'), timeoutMs, 'successful consent-gated API/event probe');
        const responseElapsed = performance.now() - pendingStarted;
        if (responseElapsed < 300) problems.push(`host probe wait ${Math.round(responseElapsed)}ms, expected at least 300ms`);
        const probeAt = running.lines.indexOf(complete);
        const releaseAt = running.lines.findIndex(line => line.startsWith('ALEF_E2E consent-released'));
        if (!(pendingAt < releaseAt && releaseAt < probeAt)) problems.push('consent markers were not ordered pending < release < probe');
        if (!/path=\S+ info=ok appInfo=ok event=ok$/.test(complete)) problems.push(`probe did not verify path/os.info/app.info/event: ${complete}`);
        const verdictLine = await running.waitFor(line => line === 'ALEF_E2E RESULT PASS bus-consent-probe', timeoutMs, 'PASS probe verdict');
        if (!verdictLine || running.lines.indexOf(verdictLine) <= probeAt) problems.push('probe PASS verdict did not follow successful probe');
        if (problems.length === 0) {
          console.log(`    ${BUS_XFAIL}`);
          lines.push(BUS_XFAIL);
        }
      } catch (error) {
        problems.push(error.message);
      } finally {
        if (running) {
          running.stop();
          if (!(await running.waitForClosed(5000))) problems.push('consent runtime log pipe did not close within 5000ms');
          lines.push(...running.lines);
        }
        rmSync(home, { recursive: true, force: true });
      }
      return { problems, lines };
    },
  };
}

