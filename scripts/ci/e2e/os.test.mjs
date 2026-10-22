// SPDX-License-Identifier: MIT OR Apache-2.0
// node --test scripts/ci/e2e/os.test.mjs: the canonical groups stay in lockstep with the CI steps.
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';
import { E2E_GROUPS, expandGroup } from './groups.mjs';

const script = fileURLToPath(new URL('./os.mjs', import.meta.url));
const ciYml = readFileSync(new URL('../../../.github/workflows/ci.yml', import.meta.url), 'utf8');
const runMjs = readFileSync(new URL('../../../tests/e2e/run.mjs', import.meta.url), 'utf8');
const affected = readFileSync(new URL('../affected.json', import.meta.url), 'utf8');

test('the_apps_ui_scenario_is_selected_for_backend_assets_and_scenario_changes', () => {
  const rules = JSON.parse(affected).e2e_rules;
  assert.ok(runMjs.includes('...appsUiScenarios({ exe, verbose })'));
  for (const changed of ['backend/crates/alef/apps/window.rs', 'backend/crates/alef/assets/apps/apps.js', 'tests/e2e/scenarios/consent.mjs']) {
    const key = changed.startsWith('backend/crates/alef/assets/apps/') ? 'backend/crates/alef/assets/apps/**' : changed;
    assert.ok(rules.some(rule => rule.scenarios.includes('apps-ui') && rule.glob === key), changed);
  }
});

test('every_group_scenario_is_known_to_the_runner', () => {
  const known = runMjs.match(/--only ([a-z0-9,-]+)\]/)?.[1].split(',') ?? [];
  for (const group of E2E_GROUPS) {
    for (const scenario of group.split(',')) assert.ok(known.includes(scenario), `unknown scenario: ${scenario}`);
  }
});

test('the_windows_steps_use_exactly_the_canonical_groups', () => {
  const steps = [...ciYml.matchAll(/--group "([^"]+)"/g)].map(m => expandGroup(m[1]).split(','));
  assert.deepEqual(steps, E2E_GROUPS.map(group => group.split(',')));
});

test('the_union_of_the_groups_equals_the_runner_scenario_list_without_duplicates', () => {
  const known = runMjs.match(/--only ([a-z0-9,-]+)\]/)?.[1].split(',') ?? [];
  const union = [];
  for (const group of E2E_GROUPS) {
    const scenarios = group.split(',');
    assert.equal(new Set(scenarios).size, scenarios.length, `duplicate in group: ${group}`);
    for (const scenario of scenarios) {
      assert.ok(!union.includes(scenario), `scenario in two groups: ${scenario}`);
      union.push(scenario);
    }
  }
  // The runner's usage line orders scenarios differently; the groups must cover the same set.
  assert.deepEqual([...union].sort(), [...known].sort());
});

test('the_macos_step_block_pins_the_condition_bound_and_command', () => {
  const block = ciYml.match(/- name: e2e \(alef on Servo, macOS[\s\S]*?(?=\n      - name: )/)?.[0];
  assert.ok(block !== undefined, 'macOS e2e step not found');
  assert.ok(block.includes("if: runner.os == 'macOS' && needs.plan.outputs.e2e != 'none'"));
  assert.ok(block.includes('timeout-minutes: 180'));
  assert.ok(block.includes('shell: bash'));
  assert.ok(block.includes('run: node scripts/ci/e2e/os.mjs --plan "${{ needs.plan.outputs.e2e }}" --exe backend/target/debug/alef --timeout-s 150'));
});

test('the_runner_receives_the_exact_only_list_exe_and_timeout', async () => {
  const dir = await mkdtemp(path.join(tmpdir(), 'alef-e2e-os-'));
  try {
    const stub = path.join(dir, 'runner.mjs');
    await writeFile(stub, "console.log(JSON.stringify(process.argv.slice(2)));");
    const result = spawnSync(process.execPath, [script, '--plan', 'fs,updater', '--exe', 'bin/alef', '--timeout-s', '120'], {
      encoding: 'utf8', timeout: 10000, maxBuffer: 1024 * 1024,
      env: { ...process.env, ALEF_E2E_GROUP_RUNNER: stub },
    });
    assert.equal(result.error, undefined);
    assert.equal(result.signal, null);
    assert.equal(result.status, 0, result.stderr);
    // The helper interleaves its own status lines; only the stub's JSON lines are invocations.
    const invocations = result.stdout.split('\n').filter(line => line.startsWith('[')).map(line => JSON.parse(line));
    assert.equal(invocations.length, 2);
    for (const expected of [['fs'], ['updater']]) {
      const argv = invocations.find(a => a[a.indexOf('--only') + 1] === expected.join(','));
      assert.ok(argv !== undefined, `no invocation for ${expected}`);
      assert.deepEqual(argv[argv.indexOf('--only') + 1].split(','), expected);
      assert.equal(argv[argv.indexOf('--exe') + 1], 'bin/alef');
      assert.equal(argv[argv.indexOf('--timeout-s') + 1], '120');
    }
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

test('a_group_that_hangs_until_its_deadline_fails_and_the_run_continues', async () => {
  const dir = await mkdtemp(path.join(tmpdir(), 'alef-e2e-os-'));
  try {
    const stub = path.join(dir, 'runner.mjs');
    await writeFile(stub, "setTimeout(() => {}, 60000);");
    const result = spawnSync(process.execPath, [script, '--plan', 'fs', '--exe', 'alef', '--timeout-s', '1'], {
      encoding: 'utf8', timeout: 20000, maxBuffer: 1024 * 1024,
      env: { ...process.env, ALEF_E2E_GROUP_RUNNER: stub, ALEF_E2E_GROUP_MARGIN_MS: '500' },
    });
    assert.equal(result.error, undefined);
    assert.equal(result.signal, null);
    assert.equal(result.status, 1, result.stderr);
    assert.match(result.stdout, /^group fs[^:\n]*: FAIL \(timeout after \d+ s\)$/m);
    assert.ok(result.stderr.includes('e2e groups failed: fs'), result.stderr);
    assert.ok(!result.stdout.includes('e2e: every planned group passed'), result.stdout);
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

test('a_failing_group_does_not_stop_the_later_groups_and_the_run_fails', async () => {
  const dir = await mkdtemp(path.join(tmpdir(), 'alef-e2e-os-'));
  try {
    const stub = path.join(dir, 'runner.mjs');
    await writeFile(stub, "if (process.argv.includes('--only') && process.argv[process.argv.indexOf('--only')+1].split(',')[0] === 'core') process.exit(1);");
    const result = spawnSync(process.execPath, [script, '--plan', 'all', '--exe', 'alef', '--timeout-s', '150'], {
      encoding: 'utf8', timeout: 10000, maxBuffer: 1024 * 1024,
      env: { ...process.env, ALEF_E2E_GROUP_RUNNER: stub },
    });
    assert.equal(result.error, undefined);
    assert.equal(result.signal, null);
    assert.equal(result.status, 1, result.stderr);
    assert.ok(result.stdout.includes('group core,bus: FAIL'), result.stdout);
    assert.ok(result.stdout.includes('group window: PASS'), result.stdout);
    assert.ok(result.stderr.includes('e2e groups failed: core,bus'), result.stderr);
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

test('groups_outside_the_plan_are_skipped_and_at_least_one_group_runs', async () => {
  const dir = await mkdtemp(path.join(tmpdir(), 'alef-e2e-os-'));
  try {
    const stub = path.join(dir, 'runner.mjs');
    await writeFile(stub, "console.log('ran ' + process.argv[process.argv.indexOf('--only')+1]);");
    const result = spawnSync(process.execPath, [script, '--plan', 'fs', '--exe', 'alef', '--timeout-s', '150'], {
      encoding: 'utf8', timeout: 10000, maxBuffer: 1024 * 1024,
      env: { ...process.env, ALEF_E2E_GROUP_RUNNER: stub },
    });
    assert.equal(result.error, undefined);
    assert.equal(result.signal, null);
    assert.equal(result.status, 0, result.stderr);
    assert.ok(result.stdout.includes('ran fs'), result.stdout);
    assert.equal((result.stdout.match(/nothing planned/g) ?? []).length, E2E_GROUPS.length - 1);
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

test('a_plan_of_none_runs_no_group', async () => {
  const dir = await mkdtemp(path.join(tmpdir(), 'alef-e2e-os-'));
  try {
    const stub = path.join(dir, 'runner.mjs');
    await writeFile(stub, "process.exit(7);");
    const result = spawnSync(process.execPath, [script, '--plan', 'none', '--exe', 'alef', '--timeout-s', '150'], {
      encoding: 'utf8', timeout: 10000, maxBuffer: 1024 * 1024,
      env: { ...process.env, ALEF_E2E_GROUP_RUNNER: stub },
    });
    assert.equal(result.error, undefined);
    assert.equal(result.signal, null);
    assert.equal(result.status, 0, result.stderr);
    assert.equal((result.stdout.match(/nothing planned/g) ?? []).length, E2E_GROUPS.length);
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});
