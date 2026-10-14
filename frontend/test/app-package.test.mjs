// SPDX-License-Identifier: MIT OR Apache-2.0
// pack:app packs the built File Manager like any Alef application through @alef-runtime/tools:
// the script is plain node (cross-platform), requires the key name from the environment and
// refuses to run without a built application directory.
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { readFileSync } from 'node:fs';
import { dirname, join, resolve } from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';

const here = dirname(fileURLToPath(import.meta.url));
const root = resolve(here, '..', '..');
const script = join(root, 'scripts', 'app', 'pack.mjs');

test('pack_app_is_a_plain_node_script_that_never_carries_a_key', () => {
  const pkg = JSON.parse(readFileSync(join(root, 'package.json'), 'utf8'));
  assert.equal(pkg.scripts['pack:app'], 'node scripts/app/pack.mjs');
  assert.ok(!readFileSync(join(root, 'package.json'), 'utf8').includes('BEGIN '), 'no key material in package.json');
});

test('pack_app_refuses_to_run_without_a_valid_key_name_and_without_a_built_app', () => {
  const base = { ...process.env };
  delete base.ALEF_RUNTIME_KEY;
  const withoutKey = spawnSync(process.execPath, [script], { encoding: 'utf8', env: base });
  assert.notEqual(withoutKey.status, 0);
  assert.ok(withoutKey.stderr.includes('ALEF_RUNTIME_KEY'));
  const badKey = spawnSync(process.execPath, [script], { encoding: 'utf8', env: { ...process.env, ALEF_RUNTIME_KEY: 'bad key!' } });
  assert.notEqual(badKey.status, 0);
  const noApp = spawnSync(process.execPath, [script], { encoding: 'utf8', env: { ...process.env, ALEF_RUNTIME_KEY: 'ci' } });
  assert.notEqual(noApp.status, 0);
  const built = spawnSync('node', ['-e', `process.exit(require('fs').existsSync(${JSON.stringify(join(root, 'frontend', 'app', 'alef.ktav'))}) ? 0 : 1)`]);
  if (built.status !== 0) assert.ok(noApp.stderr.includes('alef.ktav'));
});
