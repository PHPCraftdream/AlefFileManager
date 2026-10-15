// SPDX-License-Identifier: MIT OR Apache-2.0
// What the user sees while the application starts (Windows: the probe watches the windows of the
// process): no helper window flashes, and the main window is shown with content, not as an
// unpainted rectangle (white, with a black strip where it grew to its size).
import { spawnSync } from 'node:child_process';
import { join } from 'node:path';

import { here, prepareSite, quiet, scratch } from '../lib.mjs';

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

const BUS_XFAIL = 'XFAIL bus-runtime-consent-remains-a-prelaunch-gate-audit-160';
export function busScenarios({ drive }) {
  return {
    bus: () => drive({
      name: 'bus', app: 'modules/system/bus', expectedChecks: [
        'bus-warm-api-calls', 'bus-executor-entered', 'bus-unrelated-calls-while-blocked',
        'bus-event-roundtrip-while-blocked', 'bus-capacity-one-is-busy', 'bus-blocking-duration-bounded',
        'bus-invalid-duration-rejected', 'bus-negative-control-stalls',
        'bus-runtime-consent-remains-a-prelaunch-gate-audit-160',
      ],
      expectedBusStalls: [{ kind: 'call/e2e.blockBus', minMs: 150, maxMs: 5000, count: 1, after: 'ALEF_E2E bus-negative-begin', before: 'ALEF_E2E bus-negative-end' }],
      judge: lines => lines.some(line => line.includes(BUS_XFAIL)) ? [] : [`missing explicit ${BUS_XFAIL} audit`],
    }),
  };
}
