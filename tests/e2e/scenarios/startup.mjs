// SPDX-License-Identifier: MIT OR Apache-2.0
// What the user sees while the application starts (Windows: the probe watches the windows of the
// process): no helper window flashes, and the main window is shown with content, not as an
// unpainted rectangle (white, with a black strip where it grew to its size).
import { spawnSync } from 'node:child_process';
import { rmSync, writeFileSync } from 'node:fs';
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
        const stalls = running.lines.filter(line => line.startsWith('ALEF_BUS_STALL'));
        if (stalls.length > 0) problems.push(`the consent launch raised bus stall diagnostics: ${stalls.join(' | ')}`);
        if (problems.length === 0) {
          lines.push('consent launch: probes held until release, then finished without a bus stall');
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


/** Runs per mode; kept small so the scenario stays within its e2e group's budget on slow runners. */
export const RUNS = 9;
/** Marks every windowed run must print; `first-frame` is only noted when it comes (the quiet e2e windows may never present one). */
export const REQUIRED_WINDOWED = ['main', 'ready', 'first-info', 'context-ready', 'servo-built', 'webview-built', 'load-complete'];
/** Marks every windowless run must print. */
export const REQUIRED_WINDOWLESS = ['main', 'ready', 'first-info'];
/** Canonical order of the marks of the cold path (docs/stages/m11-warm-start.md). */
export const COLD_PATH = ['main', 'args-parsed', 'settle-begin', 'settle-end', 'tls-installed', 'bridge-ready', 'ready', 'context-ready', 'servo-built', 'webview-built', 'load-complete', 'first-info', 'first-frame'];

// The page of the timing site: one `app.info` (the `first-info` mark); the runner stops the process
// itself once the marks it needs have been seen.
const TIMING_PAGE = [
  '// Startup-timing site: one app.info for the first-info mark; the runner stops the process.',
  "import { api } from './harness.js';",
  'try { await api.app.info(); } catch { /* the mark is what matters, not the reply */ }',
  '',
].join('\n');

/** A prepared scenario site whose page only calls `app.info` and idles. */
function timingSite(name, app) {
  const site = prepareSite(name, app);
  writeFileSync(join(site, 'dist', 'page.js'), TIMING_PAGE);
  return site;
}

/** One `ALEF_TIMING <name> <ms>` line, in order of appearance. */
export function parseTimings(lines) {
  const marks = [];
  for (const line of lines) {
    const match = /^ALEF_TIMING (\S+) (\d+(?:\.\d+)?)$/.exec(line.trim());
    if (match) marks.push({ name: match[1], ms: Number(match[2]) });
  }
  return marks;
}

/** Structural verdict on one run: required marks, once each, values never decreasing. */
export function checkMarks(marks, windowed, requiredWindowed = REQUIRED_WINDOWED, requiredWindowless = REQUIRED_WINDOWLESS) {
  const problems = [];
  const seen = new Map();
  for (const { name, ms } of marks) {
    if (seen.has(name)) problems.push(`mark ${name} printed twice (${seen.get(name)} and ${ms} ms)`);
    seen.set(name, ms);
  }
  for (let index = 1; index < marks.length; index++) {
    if (marks[index].ms < marks[index - 1].ms) {
      problems.push(`marks are not monotonic: ${marks[index - 1].name} ${marks[index - 1].ms} -> ${marks[index].name} ${marks[index].ms}`);
    }
  }
  for (const name of windowed ? requiredWindowed : requiredWindowless) {
    if (!seen.has(name)) problems.push(`mark ${name} is missing`);
  }
  return problems;
}

/** Linear-interpolation percentile of an unsorted array (p in 0..1). */
export function percentile(values, p) {
  if (values.length === 0) return NaN;
  const sorted = [...values].sort((left, right) => left - right);
  const at = (sorted.length - 1) * p;
  const low = Math.floor(at);
  const high = Math.ceil(at);
  return sorted[low] + (sorted[high] - sorted[low]) * (at - low);
}

const spread = values => ({
  n: values.length,
  p50: percentile(values, 0.5),
  p95: percentile(values, 0.95),
  min: Math.min(...values),
  max: Math.max(...values),
});

const round = value => Number(value.toFixed(1));

/**
 * Table of milestones and of the phases between neighbouring marks: `runs` is an array of mark
 * arrays (one per run, in print order). A milestone a run did not print is simply absent from its
 * sample; the numbers stay comparable because a missing mark means a missing phase, not a zero.
 */
export function summarize(runs) {
  const byName = new Map();
  for (const marks of runs) {
    for (const { name, ms } of marks) {
      if (!byName.has(name)) byName.set(name, []);
      byName.get(name).push(ms);
    }
  }
  const milestones = new Map([...byName.entries()]
    .map(([name, values]) => [name, spread(values)]));
  const phases = new Map();
  for (const marks of runs) {
    for (let index = 1; index < marks.length; index++) {
      const key = `${marks[index - 1].name} -> ${marks[index].name}`;
      const delta = marks[index].ms - marks[index - 1].ms;
      if (!phases.has(key)) phases.set(key, []);
      phases.get(key).push(delta);
    }
  }
  const phaseStats = new Map([...phases.entries()].map(([key, values]) => [key, spread(values)]));
  return { milestones, phases: phaseStats };
}

const header = ['milestone', 'n', 'p50', 'p95', 'min', 'max'];

function table(rows) {
  const width = column => Math.max(...[header[column], ...rows.map(row => String(row[column]))].map(cell => cell.length));
  const line = row => `| ${header.map((_, column) => String(row[column]).padEnd(width(column))).join(' | ')} |`;
  return [line(header), `| ${header.map((_, column) => '-'.repeat(width(column))).join(' | ')} |`, ...rows.map(line)];
}

const statRows = (stats, nameOf) => [...stats.entries()]
  .sort((left, right) => COLD_PATH.indexOf(nameOf(left)) - COLD_PATH.indexOf(nameOf(right)))
  .map(([key, item]) => [nameOf({ key }), item.n, round(item.p50), round(item.p95), round(item.min), round(item.max)]);

/** Readable stdout table of one mode's summary. */
export function summaryText(mode, summary) {
  const milestoneRows = statRows(summary.milestones, ({ key }) => key);
  const phaseRows = statRows(summary.phases, ({ key }) => `phase ${key}`);
  return [`startup-timing ${mode}: milestone ms from process start`, ...table(milestoneRows),
    `startup-timing ${mode}: phase ms between neighbouring marks`, ...table(phaseRows)].join('\n');
}

/** Markdown block for $GITHUB_STEP_SUMMARY: OS, mode and N in the heading, the same tables. */
export function summaryMarkdown(os, mode, runsCount, summary) {
  const milestoneRows = statRows(summary.milestones, ({ key }) => ('`' + key + '`'));
  const phaseRows = statRows(summary.phases, ({ key }) => ('`' + key + '`'));
  const section = (title, rows) => [`### startup-timing — ${os} — ${mode} — N=${runsCount} — ${title}`, ...table(rows)].join('\n');
  return [section('milestones (ms from process start)', milestoneRows), section('phases (ms between neighbouring marks)', phaseRows)].join('\n\n');
}

export function startupTimingScenarios({ exe, verbose, timeoutMs }) {
  const waitMs = Math.min(timeoutMs, 120000);
  // Windowed: the manifest of `modules/app/basic` declares windows; windowless: the console one
  // has none. Both pages are replaced, so only the mode of the manifest differs.
  const MODES = [
    { mode: 'windowed', windowed: true, app: 'modules/app/basic', site: 'startup-timing-windowed' },
    { mode: 'windowless', windowed: false, app: 'modules/app/console', site: 'startup-timing-windowless' },
  ];

  async function timedRun(spec) {
    const site = timingSite(spec.site, spec.app);
    const running = startApp({ exe, args: ['--app', site], verbose, env: { ALEF_TIMING: '1' } });
    try {
      await running.waitFor(line => line.startsWith('ALEF_TIMING first-info '), waitMs, 'the ALEF_TIMING first-info mark');
      // The first frame is a bonus: a hidden e2e window may never present one.
      if (spec.windowed) await running.waitFor(line => line.startsWith('ALEF_TIMING first-frame '), 5000, 'the ALEF_TIMING first-frame mark').catch(() => {});
    } finally {
      running.stop();
      if (!(await running.waitForClosed(5000))) {
        console.log(`    the timing process did not let go of its log pipe: ${running.stuckReport()}`);
      }
    }
    return { marks: parseTimings(running.lines), stderr: running.stderr().toString() };
  }

  async function measure(spec) {
    const problems = [];
    const runs = [];
    for (let index = 0; index < RUNS; index++) {
      const run = await timedRun(spec);
      for (const problem of checkMarks(run.marks, spec.windowed)) {
        problems.push(`run ${index + 1}/${RUNS}: ${problem}\n--- stderr of the run ---\n${run.stderr}--- end of stderr ---`);
      }
      runs.push(run.marks);
    }
    const summary = summarize(runs);
    console.log(summaryText(spec.mode, summary));
    return { problems, summary, runsCount: runs.length };
  }

  return {
    async 'startup-timing'() {
      const problems = [];
      const summaries = [];
      for (const spec of MODES) {
        const outcome = await measure(spec);
        outcome.mode = spec.mode;
        problems.push(...outcome.problems.map(problem => `${spec.mode}: ${problem}`));
        summaries.push(outcome);
      }
      const summaryFile = process.env.GITHUB_STEP_SUMMARY;
      if (summaryFile) {
        const os = `${process.platform}/${process.arch}`;
        writeFileSync(summaryFile, `${summaries.map(outcome => summaryMarkdown(os, outcome.mode, RUNS, outcome.summary)).join('\n\n')}\n`, { flag: 'a' });
      }
      return { problems, lines: [] };
    },
  };
}
