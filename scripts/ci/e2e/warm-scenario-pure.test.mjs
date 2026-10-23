// SPDX-License-Identifier: MIT OR Apache-2.0
// node --test scripts/ci/e2e/warm-scenario-pure.test.mjs: the pure helpers of the warm e2e
// scenario (scripts/ci/e2e runs it in the first Windows group) — the status JSON shape, the slot
// lookup, the shared session env fragment and the cold/warm timing table. Pure module: no runtime,
// no broker, no process is involved.
import assert from 'node:assert/strict';
import test from 'node:test';

import {
  modeTable, parseStatusJson, slotState, warmEnvironment,
} from '../../../tests/e2e/scenarios/app/warm/warm.mjs';
import { percentile } from '../../../tests/e2e/scenarios/startup.mjs';

const status = {
  broker: {
    running: true,
    pid: 4172,
    version: '0.1.0',
    granted_total: 3,
    running_apps: 1,
    slots: [
      { kind: 'windowed', state: 'ready' },
      { kind: 'windowless', state: 'empty' },
    ],
  },
  autostart: 'disabled',
};

test('parseStatusJson_reads_the_broker_object_and_refuses_garbage', () => {
  assert.deepEqual(parseStatusJson(JSON.stringify(status)), status);
  const down = { broker: { running: false }, autostart: 'enabled' };
  assert.deepEqual(parseStatusJson(JSON.stringify(down)), down);
  for (const garbage of ['', 'broker: not running', 'null', '[1,2]', '{"no-broker":true}', '{"broker":', '[]']) {
    assert.equal(parseStatusJson(garbage), null, garbage);
  }
});

test('slotState_finds_both_slots_and_is_undefined_without_them', () => {
  assert.equal(slotState(status, 'windowed'), 'ready');
  assert.equal(slotState(status, 'windowless'), 'empty');
  assert.equal(slotState(status, 'unknown'), undefined);
  assert.equal(slotState({ broker: { running: false } }, 'windowed'), undefined);
  assert.equal(slotState(null, 'windowed'), undefined);
});

test('warmEnvironment_carries_the_store_identity_and_the_unix_runtime_dir_only', () => {
  const dirs = { home: '/tmp/h', appHome: '/tmp/a', runtimeDir: '/tmp/r' };
  assert.deepEqual(warmEnvironment(dirs, 'linux'), { ALEF_HOME: '/tmp/h', ALEF_APP_HOME: '/tmp/a', XDG_RUNTIME_DIR: '/tmp/r' });
  assert.deepEqual(warmEnvironment(dirs, 'darwin'), { ALEF_HOME: '/tmp/h', ALEF_APP_HOME: '/tmp/a', XDG_RUNTIME_DIR: '/tmp/r' });
  // Windows: the endpoint is a pipe name derived from the identity; no XDG variable exists there.
  assert.deepEqual(warmEnvironment(dirs, 'win32'), { ALEF_HOME: '/tmp/h', ALEF_APP_HOME: '/tmp/a' });
  // No runtime dir given: the fragment stays complete apart from it.
  const withoutRuntime = { home: '/h', appHome: '/a' };
  assert.deepEqual(warmEnvironment(withoutRuntime, 'linux'), { ALEF_HOME: '/h', ALEF_APP_HOME: '/a' });
});

test('the_percentile_import_behaves_like_the_startup_scenario_one', () => {
  assert.equal(percentile([10, 20, 30, 40], 0.5), 25);
  assert.equal(percentile([7], 0.95), 7);
});

test('modeTable_reports_n_p50_p95_min_max_for_both_modes', () => {
  const rows = modeTable([10, 20, 30, 40], [100, 200]);
  assert.equal(rows.length, 2);
  assert.deepEqual(
    { mode: 'cold', n: 4, p50: 25, p95: 38.5, min: 10, max: 40 },
    { mode: rows[0].mode, n: rows[0].n, p50: rows[0].p50, p95: rows[0].p95, min: rows[0].min, max: rows[0].max },
  );
  assert.deepEqual(
    { mode: 'warm', n: 2, p50: 150, p95: 195, min: 100, max: 200 },
    { mode: rows[1].mode, n: rows[1].n, p50: rows[1].p50, p95: rows[1].p95, min: rows[1].min, max: rows[1].max },
  );
  // Empty samples do not throw: the numbers are NaN, the shape stays.
  const empty = modeTable([], [5]);
  assert.equal(empty[0].n, 0);
  assert.ok(Number.isNaN(empty[0].p50));
  assert.equal(empty[1].min, 5);
});
