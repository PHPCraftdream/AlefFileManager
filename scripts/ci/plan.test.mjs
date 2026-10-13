// SPDX-License-Identifier: MIT OR Apache-2.0
// node --test scripts/ci/plan.test.mjs: the planner's pure logic and the affected.json data stay conservative.
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { spawnSync } from 'node:child_process';
import test from 'node:test';
import { fileURLToPath } from 'node:url';
import path from 'node:path';
import { affectedJs, applyOverrides, computePlan, e2eFor, isDocsOnly, matchGlob, matchesAny, reverseDependents } from './plan.mjs';

const root = fileURLToPath(new URL('../../', import.meta.url));
const data = JSON.parse(readFileSync(path.join(root, 'scripts', 'ci', 'affected.json'), 'utf8'));

const graph = {
  'alef': ['alef-core', 'alef-modules', 'alef-package', 'alef-runtime'],
  'alef-modules': ['alef-core'],
  'alef-runtime': ['alef-core', 'alef-package'],
  'alef-core': [],
  'alef-package': ['alef-runtime'],
};
const allCrates = ['alef', 'alef-core', 'alef-modules', 'alef-package', 'alef-runtime'];
const pkgs = [
  { folder: 'packages/api', name: '@alef-tron/api', dependencies: {} },
  { folder: 'packages/runtime', name: '@alef-tron/runtime', dependencies: { '@alef-tron/api': '^0.1.0' } },
  { folder: 'packages/mcp', name: '@alef-tron/mcp', dependencies: {} },
];

function plan(files, overrides = {}) {
  return computePlan({
    event: 'push', ref: 'refs/heads/ci-plan', files, filesKnown: true, message: '', manualFull: false,
    runnersAll: data.runners_all, runnersLinux: data.runners_linux,
    fullTriggers: data.full_triggers, platformSensitive: data.platform_sensitive,
    e2eRules: data.e2e_rules, e2eSmoke: data.e2e_smoke,
    allCrates, crateGraph: graph, packages: pkgs, ...overrides,
  });
}

test('a_docs_only_change_runs_only_structure_and_fmt_on_linux_with_no_crates_js_or_e2e', () => {
  const p = plan(['docs/stages/m0-results/ci.md']);
  assert.equal(p.full, false);
  assert.equal(p.docs_only, true);
  assert.deepEqual(p.os, ['ubuntu-24.04']);
  assert.deepEqual(p.crates, []);
  assert.deepEqual(p.js, []);
  assert.equal(p.e2e, 'none');
  assert.equal(p.lint, true);
  assert.equal(p.structure, true);
  assert.equal(p.fmt, true);
});

test('a_change_in_one_leaf_crate_tests_only_that_crate_its_dependents_and_linux', () => {
  const p = plan(['backend/crates/alef-package/src/lib.rs']);
  assert.equal(p.full, false);
  assert.deepEqual(p.crates, ['alef', 'alef-package', 'alef-runtime']);
  assert.equal(p.crates_all, false);
  assert.deepEqual(p.os, ['ubuntu-24.04']);
});

test('a_change_in_alef_core_tests_every_crate_in_the_workspace', () => {
  const p = plan(['backend/crates/alef-core/src/lib.rs']);
  for (const crate of allCrates) assert.ok(p.crates.includes(crate), crate);
});

test('a_platform_sensitive_change_runs_all_four_operating_systems_without_full', () => {
  const p = plan(['backend/crates/alef/src/relaunch/windows.rs']);
  assert.deepEqual(p.os, data.runners_all);
  assert.equal(p.full, false);
  assert.equal(p.e2e, 'quit,relaunch');
  const q = plan(['backend/crates/alef-core/src/something_unix.rs']);
  assert.deepEqual(q.os, data.runners_all);
  assert.equal(q.full, false);
});

test('a_workflow_change_runs_the_full_plan', () => {
  const p = plan(['.github/workflows/ci.yml']);
  assert.equal(p.full, true);
  assert.deepEqual(p.os, data.runners_all);
  assert.equal(p.crates, 'all');
  assert.deepEqual(p.js, ['all']);
  assert.equal(p.e2e, 'all');
});

test('a_push_to_main_runs_the_full_plan', () => {
  const p = plan(['backend/crates/alef-core/src/lib.rs'], { ref: 'refs/heads/main' });
  assert.equal(p.full, true);
});

test('a_full_marker_in_the_commit_message_runs_the_full_plan', () => {
  const p = plan(['docs/x.md'], { message: 'tweak [full] later' });
  assert.equal(p.full, true);
  assert.equal(p.e2e, 'all');
});

test('an_unknown_changed_file_list_falls_back_to_the_full_plan', () => {
  const p = plan([], { filesKnown: false });
  assert.equal(p.full, true);
  assert.deepEqual(p.os, data.runners_all);
  assert.equal(p.crates, 'all');
  assert.deepEqual(p.js, ['all']);
  assert.equal(p.e2e, 'all');
});

test('a_changed_package_json_of_one_js_package_runs_that_package_and_its_dependents', () => {
  const js = affectedJs(['packages/api/src/index.ts'], pkgs, null, false);
  assert.deepEqual(js, ['packages/api', 'packages/runtime']);
  const p = plan(['packages/api/src/index.ts']);
  assert.deepEqual(p.js, ['packages/api', 'packages/runtime']);
});

test('a_changed_e2e_scenario_file_maps_to_the_smoke_set', () => {
  const p = plan(['tests/e2e/scenarios/consent.mjs']);
  assert.equal(p.e2e, 'consent,core,csp,manifest,permissions');
  assert.deepEqual(p.os, data.runners_all);
});

test('a_path_no_rule_knows_widens_the_plan_to_full', () => {
  const p = plan(['experiments/thing/whatever.rs']);
  assert.equal(p.full, true);
});

test('a_frontend_change_maps_to_frontend_js_but_widens_crates_to_all', () => {
  const p = plan(['frontend/src/app.tsx']);
  assert.equal(p.e2e, 'app,console');
  assert.deepEqual(p.js, ['frontend']);
  assert.equal(p.crates, 'all');
});

test('a_manual_dispatch_with_full_false_runs_the_affected_plan_not_the_full_one', () => {
  const p = plan(['backend/crates/alef-core/src/lib.rs'], { manualFull: false });
  assert.equal(p.full, false);
});

test('a_changed_root_readme_md_is_docs_only_not_an_unknown_path', () => {
  const p = plan(['README.md']);
  assert.equal(p.docs_only, true);
  assert.equal(p.full, false);
  assert.equal(p.e2e, 'none');
});

test('the_cli_parses_full_false_as_an_affected_plan_not_full', () => {
  const planPath = fileURLToPath(new URL('./plan.mjs', import.meta.url));
  const out = spawnSync(process.execPath, [planPath, '--full', 'false', '--files', 'backend/crates/alef-core/src/lib.rs'], { encoding: 'utf8' });
  assert.equal(JSON.parse(out.stdout.slice(out.stdout.indexOf('{'))).full, false);
});

test('applyOverrides_on_a_affected_plan_replaces_the_crate_list_and_sets_crates_all_false_and_keeps_linux', () => {
  const p = plan(['backend/crates/alef-package/src/lib.rs']);
  const out = applyOverrides(p, { crates: ' alef-modules, alef-modules ' }, data.runners_all);
  assert.deepEqual(out.crates, ['alef-modules']);
  assert.equal(out.crates_all, false);
  assert.deepEqual(out.os, ['ubuntu-24.04']);
});

test('applyOverrides_with_an_empty_crates_value_leaves_the_plan_untouched_except_e2e_handling', () => {
  const p = plan(['backend/crates/alef-package/src/lib.rs']);
  const out = applyOverrides(p, { crates: '', e2e: '' }, data.runners_all);
  assert.deepEqual(out, p);
});

test('applyOverrides_with_an_e2e_list_widens_os_to_all_runners_and_replaces_the_scenarios', () => {
  const p = plan(['backend/crates/alef-package/src/lib.rs']);
  const out = applyOverrides(p, { e2e: 'consent' }, data.runners_all);
  assert.equal(out.e2e, 'consent');
  assert.deepEqual(out.os, data.runners_all);
});

test('applyOverrides_with_e2e_none_disables_e2e_but_keeps_the_operating_systems_of_the_plan', () => {
  const p = plan(['tests/e2e/scenarios/consent.mjs']);
  const out = applyOverrides(p, { e2e: 'none' }, data.runners_all);
  assert.equal(out.e2e, 'none');
  assert.deepEqual(out.os, p.os);
});

test('applyOverrides_on_a_full_plan_is_not_applied_by_main', () => {
  const planPath = fileURLToPath(new URL('./plan.mjs', import.meta.url));
  const out = spawnSync(process.execPath, [planPath, '--event', 'push', '--ref', 'refs/heads/main', '--files', 'scripts/ci/plan.mjs', '--crates', 'alef-core'], { cwd: root, encoding: 'utf8' });
  const parsed = JSON.parse(out.stdout.slice(out.stdout.indexOf('{')));
  assert.equal(parsed.full, true);
  assert.equal(parsed.crates, 'all');
});

test('the_cli_applies_crates_and_e2e_overrides_on_an_affected_plan', () => {
  const planPath = fileURLToPath(new URL('./plan.mjs', import.meta.url));
  const out = spawnSync(process.execPath, [planPath, '--event', 'push', '--ref', 'refs/heads/x', '--files', 'backend/crates/alef-core/src/lib.rs', '--crates', 'alef-core', '--e2e', 'core'], { cwd: root, encoding: 'utf8' });
  const parsed = JSON.parse(out.stdout.slice(out.stdout.indexOf('{')));
  assert.deepEqual(parsed.crates, ['alef-core']);
  assert.equal(parsed.e2e, 'core');
});

test('matchGlob_matches_deep_wildcards_but_not_similar_prefixes_and_empty_files_are_not_docs_only', () => {
  assert.equal(matchGlob('tests/e2e/scenarios/consent.mjs', 'tests/e2e/**'), true);
  assert.equal(matchGlob('tests/e2eX/y', 'tests/e2e/**'), false);
  assert.equal(matchesAny('backend/crates/alef/src/relaunch/windows.rs', ['**/*windows*']), true);
  assert.equal(isDocsOnly([]), false);
});
