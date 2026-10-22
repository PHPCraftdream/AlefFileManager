// SPDX-License-Identifier: MIT OR Apache-2.0
// node --test scripts/ci/e2e/startup-timing-stats.test.mjs: the pure statistics of the e2e
// startup-timing scenario (scripts/ci/e2e runs it in CI) — parsing the ALEF_TIMING marks, the
// structural verdict, percentiles and the summary tables. Pure module: no runtime is involved.
import assert from 'node:assert/strict';
import test from 'node:test';

import {
  checkMarks, parseTimings, percentile, RUNS, REQUIRED_WINDOWED, REQUIRED_WINDOWLESS, summarize, summaryMarkdown, summaryText,
} from '../../../tests/e2e/scenarios/startup.mjs';

const mark = (name, ms) => ({ name, ms });
const run = (...pairs) => pairs.map(([name, ms]) => mark(name, ms));

test('parseTimings_reads_name_and_milliseconds_and_ignores_everything_else', () => {
  const lines = [
    'ALEF_TIMING main 0.123', 'noise', 'ALEF_READY app=x pid=1',
    'ALEF_TIMING ready 812.5', 'ALEF_TIMING first-info 900', 'ALEF_TIMING main 0.5',
  ];
  assert.deepEqual(parseTimings(lines), [mark('main', 0.123), mark('ready', 812.5), mark('first-info', 900), mark('main', 0.5)]);
  assert.deepEqual(parseTimings([]), []);
});

test('checkMarks_accepts_a_clean_windowed_and_windowless_run', () => {
  const windowed = run(...REQUIRED_WINDOWED.map((name, index) => [name, index]));
  assert.deepEqual(checkMarks(windowed, true), []);
  const windowless = run(...REQUIRED_WINDOWLESS.map((name, index) => [name, index * 2]));
  assert.deepEqual(checkMarks(windowless, false), []);
});

test('checkMarks_reports_a_missing_mark_a_repeat_and_a_step_back', () => {
  const missing = run(['main', 0], ['ready', 5]);
  assert.ok(checkMarks(missing, false).some(problem => problem.includes('first-info is missing')));
  const repeated = run(['main', 0], ['main', 1], ['ready', 5], ['first-info', 9]);
  assert.ok(checkMarks(repeated, false).some(problem => problem.includes('main printed twice')));
  const backwards = run(['main', 0], ['ready', 5], ['first-info', 4]);
  assert.ok(checkMarks(backwards, false).some(problem => problem.includes('not monotonic')));
  assert.equal(checkMarks(run(['ready', 1], ['main', 2], ['first-info', 3]), false).length, 0);
});

test('percentile_interpolates_between_the_nearest_ranks', () => {
  assert.equal(percentile([], 0.5), NaN);
  assert.equal(percentile([7], 0.95), 7);
  assert.equal(percentile([10, 20, 30, 40], 0.5), 25);
  assert.equal(percentile([10, 20, 30, 40], 0), 10);
  assert.equal(percentile([10, 20, 30, 40], 1), 40);
  assert.equal(percentile([4, 1, 3, 2, 5], 0.95), 4.8);
});

test('summarize_collects_milestones_and_neighbouring_phases_over_runs', () => {
  const runs = [
    run(['main', 0], ['ready', 100], ['first-info', 200]),
    run(['main', 10], ['ready', 200], ['first-info', 400]),
  ];
  const summary = summarize(runs);
  assert.deepEqual([...summary.milestones.keys()], ['main', 'ready', 'first-info']);
  assert.deepEqual(
    { ...summary.milestones.get('ready'), p50: 150, p95: 190, min: 100, max: 200, n: 2 },
    { n: 2, p50: 150, p95: 190, min: 100, max: 200 },
  );
  assert.deepEqual([...summary.phases.keys()], ['main -> ready', 'ready -> first-info']);
  const phase = summary.phases.get('ready -> first-info');
  assert.equal(phase.n, 2);
  assert.equal(phase.p50, 150);
  assert.equal(phase.min, 100);
  assert.equal(phase.max, 200);
  // A run without a mark contributes nothing to it, not a zero.
  const partial = summarize([run(['main', 0], ['ready', 100]), run(['main', 5])]);
  assert.equal(partial.milestones.get('ready').n, 1);
});

test('summaryText_and_summaryMarkdown_render_the_tables_with_the_header', () => {
  const summary = summarize([run(['main', 0], ['ready', 100])]);
  const text = summaryText('windowed', summary);
  assert.ok(text.includes('startup-timing windowed'));
  assert.ok(text.includes('| ready'));
  const markdown = summaryMarkdown('linux/x64', 'windowed', RUNS, summary);
  assert.ok(markdown.startsWith('### startup-timing — linux/x64 — windowed — N=9'));
  assert.ok(markdown.includes('| p50 '));
  assert.ok(markdown.includes('— N=9 — phases'));
});
