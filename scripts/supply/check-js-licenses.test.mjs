// SPDX-License-Identifier: MIT OR Apache-2.0
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';
import { allowedLicenses, checkLock, satisfied } from './check-js-licenses.mjs';

const checker = fileURLToPath(new URL('./check-js-licenses.mjs', import.meta.url));

const DENY = `
[advisories]
ignore = [{ id = "RUSTSEC-0000-0000", reason = "no [licenses] here, allow = [\\"GPL-3.0\\"]" }]

[licenses]
# "GPL-2.0-only" in a comment is not on the list
allow = [
    "MIT",          # the usual
    "Apache-2.0",
    "Apache-2.0 WITH LLVM-exception",
    "MPL-2.0",
]

[bans]
allow = ["Unrelated"]
`;

const allowed = allowedLicenses(DENY);

test('allow_list_is_the_one_of_the_licenses_section_without_comments', () => {
  assert.deepEqual([...allowed].sort(), ['apache-2.0', 'apache-2.0 with llvm-exception', 'mit', 'mpl-2.0']);
});

test('allow_list_of_a_crlf_file_is_read_the_same', () => {
  assert.deepEqual(allowedLicenses(DENY.replace(/\n/g, '\r\n')), allowed);
});

test('allow_list_missing_or_empty_is_an_error_not_an_empty_policy', () => {
  assert.throws(() => allowedLicenses('[advisories]\nignore = []\n'), /no \[licenses\] section/);
  assert.throws(() => allowedLicenses('[licenses]\nconfidence-threshold = 0.9\n'), /no allow list/);
  assert.throws(() => allowedLicenses('[licenses]\nallow = [\n]\n'), /empty/);
});

test('real_deny_toml_has_the_allow_list', async () => {
  const real = allowedLicenses(await readFile(new URL('../../backend/deny.toml', import.meta.url), 'utf8'));
  assert.ok(real.has('mit') && real.has('apache-2.0'));
});

test('expressions_are_satisfied_by_one_side_of_or_and_both_sides_of_and', () => {
  const cases = [
    ['MIT', true],
    ['mit', true],
    ['GPL-3.0', false],
    ['MIT OR GPL-3.0', true],
    ['GPL-3.0 OR MIT', true],
    ['GPL-3.0 OR LGPL-2.1', false],
    ['MIT AND Apache-2.0', true],
    ['MIT AND GPL-3.0', false],
    ['(MIT OR GPL-3.0) AND Apache-2.0', true],
    ['(GPL-3.0 OR LGPL-2.1) AND MIT', false],
    ['MIT OR GPL-3.0 AND LGPL-2.1', true],
    ['GPL-3.0 AND LGPL-2.1 OR MIT', true],
    ['(MIT)', true],
    ['MIT or GPL-3.0', true],
  ];
  for (const [expression, expected] of cases) {
    assert.equal(satisfied(expression, allowed), expected, expression);
  }
});

test('an_exception_passes_as_a_pair_or_through_its_base_license', () => {
  assert.equal(satisfied('Apache-2.0 WITH LLVM-exception', allowed), true);
  assert.equal(satisfied('MIT WITH Classpath-exception-2.0', allowed), true);
  assert.equal(satisfied('GPL-2.0-only WITH Classpath-exception-2.0', allowed), false);
});

test('expressions_that_cannot_be_read_throw', () => {
  for (const expression of ['', 'MIT OR', 'OR MIT', '(MIT', 'MIT)', 'MIT WITH', 'MIT AND AND MIT', 'MIT Apache-2.0']) {
    assert.throws(() => satisfied(expression, allowed), Error, expression);
  }
});

function lockOf(packages) {
  return { lockfileVersion: 3, packages: { '': { name: 'root', license: 'GPL-3.0' }, ...packages } };
}

test('lock_is_clean_when_every_package_is_allowed_and_the_root_is_not_judged', () => {
  const result = checkLock(lockOf({
    'node_modules/a': { version: '1.0.0', license: 'MIT' },
    'node_modules/b': { version: '2.0.0', license: '(MIT OR GPL-3.0)' },
  }), allowed);
  assert.deepEqual(result, { checked: 2, failures: [] });
});

test('lock_names_the_package_with_a_license_outside_the_list', () => {
  const result = checkLock(lockOf({
    'node_modules/a': { version: '1.0.0', license: 'MIT' },
    'node_modules/a/node_modules/@scope/b': { version: '3.1.0', license: 'GPL-3.0' },
  }), allowed);
  assert.equal(result.checked, 2);
  assert.deepEqual(result.failures, ['@scope/b@3.1.0: GPL-3.0']);
});

test('lock_fails_a_package_without_a_license_and_one_with_an_unreadable_one', () => {
  const result = checkLock(lockOf({
    'node_modules/none': { version: '1.0.0' },
    'node_modules/blank': { version: '1.0.0', license: '  ' },
    'node_modules/legacy': { version: '1.0.0', license: { type: 'MIT' } },
    'node_modules/broken': { version: '1.0.0', license: 'MIT OR' },
  }), allowed);
  assert.equal(result.failures.length, 4);
  assert.match(result.failures[0], /^none@1\.0\.0: no license/);
  assert.match(result.failures[1], /^blank@1\.0\.0: no license/);
  assert.match(result.failures[2], /^legacy@1\.0\.0: no license/);
  assert.match(result.failures[3], /^broken@1\.0\.0: cannot read the license "MIT OR"/);
});

test('lock_skips_links_and_takes_the_name_of_a_workspace_package_from_the_entry', () => {
  const result = checkLock(lockOf({
    'node_modules/@scope/pkg': { resolved: 'packages/pkg', link: true },
    'packages/pkg': { name: '@scope/pkg', version: '0.1.0', license: 'GPL-3.0' },
  }), allowed);
  assert.deepEqual(result, { checked: 1, failures: ['@scope/pkg@0.1.0: GPL-3.0'] });
});

async function runChecker(lock, deny) {
  const directory = await mkdtemp(path.join(tmpdir(), 'alef-js-licenses-'));
  try {
    const lockPath = path.join(directory, 'package-lock.json');
    const denyPath = path.join(directory, 'deny.toml');
    await writeFile(lockPath, lock);
    await writeFile(denyPath, deny);
    const result = spawnSync(process.execPath, [checker, lockPath, denyPath], { encoding: 'utf8', timeout: 10000 });
    assert.equal(result.error, undefined);
    return result;
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
}

test('cli_exit_code_is_0_for_a_clean_lock_1_for_a_foreign_license_2_for_unreadable_inputs', async () => {
  const clean = await runChecker(JSON.stringify(lockOf({ 'node_modules/a': { version: '1.0.0', license: 'MIT' } })), DENY);
  assert.equal(clean.status, 0);
  assert.match(clean.stdout, /1 npm packages against 4 allowed licenses/);

  const foreign = await runChecker(JSON.stringify(lockOf({ 'node_modules/a': { version: '1.0.0', license: 'AGPL-3.0' } })), DENY);
  assert.equal(foreign.status, 1);
  assert.match(foreign.stderr, /not allowed: a@1\.0\.0: AGPL-3\.0/);

  const unreadable = await runChecker('{ not json', DENY);
  assert.equal(unreadable.status, 2);
  assert.match(unreadable.stderr, /cannot read the inputs/);
});
