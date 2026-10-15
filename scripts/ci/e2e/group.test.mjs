// SPDX-License-Identifier: MIT OR Apache-2.0
// node --test scripts/ci/e2e/group.test.mjs: the group selector keeps the CI e2e steps' exact contract.
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';
import { selectGroup } from './group.mjs';

const script = fileURLToPath(new URL('./group.mjs', import.meta.url));

function spawn_cli(args, env = {}) {
  const result = spawnSync(process.execPath, [script, ...args], {
    encoding: 'utf8', timeout: 5000, maxBuffer: 1024 * 1024,
    env: { ...process.env, ...env },
  });
  assert.equal(result.error, undefined);
  assert.equal(result.signal, null);
  return result;
}

test('an_empty_intersection_of_the_plan_and_the_group_selects_nothing', () => {
  assert.deepEqual(selectGroup('fs', 'core,window'), []);
});

test('a_plan_of_none_or_an_empty_string_selects_nothing', () => {
  assert.deepEqual(selectGroup('none', 'core,window'), []);
  assert.deepEqual(selectGroup('', 'core,window'), []);
});

test('a_plan_of_all_selects_every_group_scenario_in_group_order', () => {
  assert.deepEqual(selectGroup('all', 'core,window,csp'), ['core', 'window', 'csp']);
});

test('the_selection_order_follows_the_group_not_the_plan', () => {
  assert.deepEqual(selectGroup('window,core', 'core,window'), ['core', 'window']);
});

test('duplicates_in_the_group_or_the_plan_do_not_duplicate_the_selection', () => {
  assert.deepEqual(selectGroup('core,core,window', 'core,core,window'), ['core', 'window']);
});

test('unknown_plan_names_are_ignored_by_the_selection', () => {
  assert.deepEqual(selectGroup('core,ghost', 'core'), ['core']);
});

test('an_empty_intersection_exits_zero_printing_the_skip_line', () => {
  const result = spawn_cli(['--plan', 'fs', '--group', 'core,window', '--exe', 'alef.exe']);
  assert.equal(result.status, 0);
  assert.equal(result.stdout.trim(), 'no planned scenario in this group');
});

test('a_planned_scenario_spawns_the_runner_with_the_selected_list_in_group_order', async () => {
  const dir = await mkdtemp(path.join(tmpdir(), 'alef-e2e-group-'));
  try {
    const stub = path.join(dir, 'runner.mjs');
    await writeFile(stub, "console.log(JSON.stringify(process.argv.slice(2)));");
    const result = spawn_cli(
      ['--plan', 'window,csp', '--group', 'csp,window', '--exe', 'backend/target/debug/alef.exe'],
      { ALEF_E2E_GROUP_RUNNER: stub },
    );
    assert.equal(result.status, 0, result.stderr);
    const argv = JSON.parse(result.stdout);
    const only = argv[argv.indexOf('--only') + 1];
    assert.deepEqual(only.split(','), ['csp', 'window']);
    assert.equal(argv[argv.indexOf('--exe') + 1], 'backend/target/debug/alef.exe');
    assert.equal(argv[argv.indexOf('--timeout-s') + 1], '150');
    assert.ok(argv.includes('--verbose'));
    // CI never sets the test hook; the default runner is the real run.mjs relative to the script.
    const source = readFileSync(script, 'utf8');
    assert.ok(source.includes("'../../../tests/e2e/run.mjs'"));
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

test('a_plan_of_all_through_the_cli_selects_the_whole_group', async () => {
  const dir = await mkdtemp(path.join(tmpdir(), 'alef-e2e-group-'));
  try {
    const stub = path.join(dir, 'runner.mjs');
    await writeFile(stub, "console.log(JSON.stringify(process.argv.slice(2)));");
    const result = spawn_cli(
      ['--plan', 'all', '--group', 'core,window', '--exe', 'alef.exe'],
      { ALEF_E2E_GROUP_RUNNER: stub },
    );
    assert.equal(result.status, 0, result.stderr);
    const argv = JSON.parse(result.stdout);
    assert.equal(argv[argv.indexOf('--only') + 1], 'core,window');
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

test('a_missing_group_argument_exits_nonzero_with_a_stderr_message', () => {
  const result = spawn_cli(['--plan', 'core', '--exe', 'alef.exe']);
  assert.notEqual(result.status, 0);
  assert.ok(result.stderr.trim().length > 0);
});
