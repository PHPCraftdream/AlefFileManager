// SPDX-License-Identifier: MIT OR Apache-2.0
import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdirSync, readFileSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { spawnSync } from 'node:child_process';
import { resolveRuntimePath } from '../src/resolve.mjs';
import { generate, temporary } from './helpers.mjs';

test('windows_8_3_parent_aliases_work_for_override_and_generator', { skip: process.platform !== 'win32' }, t => {
  const directory = temporary(t); const long = join(directory, 'long runtime parent directory');
  mkdirSync(long); const input = join(long, 'input'); writeFileSync(input, 'short path bytes');
  const result = spawnSync(`for %I in ("${long}") do @echo %~sI`, {
    shell: true, encoding: 'utf8', timeout: 10_000, windowsHide: true,
  });
  assert.equal(result.status, 0, result.stderr);
  const alias = result.stdout.trim();
  if (!alias.includes('~')) { t.skip('8.3 names are not enabled on this volume'); return; }
  const resolved = resolveRuntimePath({ env: { ALEF_BIN: join(alias, 'input') } });
  assert.equal(readFileSync(resolved, 'utf8'), 'short path bytes');
  const generated = generate('linux-x64', join(alias, 'input'), join(alias, 'out'), '0.1.0');
  assert.equal(generated.status, 0, generated.stderr);
  assert.equal(readFileSync(join(long, 'out', 'bin', 'alef'), 'utf8'), 'short path bytes');
});
