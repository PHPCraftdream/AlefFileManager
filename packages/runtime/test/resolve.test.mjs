// SPDX-License-Identifier: MIT OR Apache-2.0
import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdirSync, readFileSync, rmSync, symlinkSync, writeFileSync } from 'node:fs';
import { isAbsolute, join, relative } from 'node:path';
import { PLATFORMS, packageName } from '../src/platforms.mjs';
import { installed, temporary } from './helpers.mjs';

for (const entry of PLATFORMS) {
  test(`resolves_installed_${entry.suffix.replaceAll('-', '_')}_beside_copied_meta_package_not_cwd`, async t => {
    const fixture = await installed(t, entry);
    const result = fixture.resolveRuntimePath({ ...entry, env: {} });
    assert.equal(result, fixture.binary);
    assert.ok(isAbsolute(result));
    assert.equal(readFileSync(result, 'utf8'), 'fake runtime');
  });
}

test('public_api_is_synchronous_argument_free_and_exports_only_get_runtime_path', async t => {
  const fixture = await installed(t, PLATFORMS.find(entry => entry.platform === process.platform && entry.arch === process.arch));
  const old = process.env.ALEF_BIN;
  const override = join(fixture.directory, 'override'); writeFileSync(override, 'override');
  try {
    process.env.ALEF_BIN = override;
    assert.equal(fixture.getRuntimePath.length, 0);
    assert.equal(fixture.getRuntimePath(), override);
  } finally {
    if (old === undefined) delete process.env.ALEF_BIN; else process.env.ALEF_BIN = old;
  }
  const api = await import('../src/index.mjs');
  assert.deepEqual(Object.keys(api), ['getRuntimePath']);
});

test('override_accepts_absolute_and_relative_regular_files_and_takes_precedence_over_platform_and_dependency', async t => {
  const fixture = await installed(t, PLATFORMS[0]);
  const file = join(fixture.directory, 'override with spaces'); writeFileSync(file, 'override');
  assert.equal(fixture.resolveRuntimePath({ platform: 'other', arch: 'other', env: { ALEF_BIN: file } }), file);
  assert.equal(fixture.resolveRuntimePath({ env: { ALEF_BIN: relative(process.cwd(), file) } }), file);
  assert.equal(fixture.resolveRuntimePath({ ...PLATFORMS[0], env: { ALEF_BIN: file } }), file);
});

test('invalid_overrides_fail_closed_never_falling_back_to_an_installed_package', async t => {
  const fixture = await installed(t, PLATFORMS[0]);
  for (const value of ['', '\0']) {
    assert.throws(() => fixture.resolveRuntimePath({ ...PLATFORMS[0], env: { ALEF_BIN: value } }), /Invalid ALEF_BIN: expected a nonempty file path/);
  }
  for (const value of [fixture.directory, join(fixture.directory, 'missing'), null]) {
    assert.throws(() => fixture.resolveRuntimePath({ ...PLATFORMS[0], env: { ALEF_BIN: value } }), /Invalid ALEF_BIN.*(?:Set ALEF_BIN|unset it)/);
  }
});

test('unsupported_platforms_and_missing_packages_identify_configuration_and_remedies', async t => {
  const fixture = await installed(t);
  assert.throws(() => fixture.resolveRuntimePath({ platform: 'linux', arch: 'arm64', env: {} }), error => {
    assert.match(error.message, /Unsupported.*linux\/arm64/);
    assert.match(error.message, /no platform package/);
    assert.match(error.message, /ALEF_BIN/);
    return true;
  });
  for (const entry of PLATFORMS) {
    assert.throws(() => fixture.resolveRuntimePath({ ...entry, env: {} }), error => {
      assert.ok(error.message.includes(packageName(entry)));
      assert.ok(error.message.includes(`${entry.platform}/${entry.arch}`));
      assert.match(error.message, /--omit=optional/);
      assert.match(error.message, /npm install --include=optional/);
      return true;
    });
  }
});

test('installed_missing_or_directory_binary_is_rejected_with_package_and_optional_dependency_fix', async t => {
  const fixture = await installed(t, PLATFORMS[0]);
  rmSync(fixture.binary);
  const check = () => assert.throws(() => fixture.resolveRuntimePath({ ...PLATFORMS[0], env: {} }), /runtime-win32-x64.*--omit=optional/);
  check(); mkdirSync(fixture.binary); check();
});

test('override_follows_a_symlinked_parent_without_lexical_canonical_equality', t => {
  const directory = temporary(t);
  const real = join(directory, 'real'); const alias = join(directory, 'alias');
  mkdirSync(real); writeFileSync(join(real, 'alef'), 'aliased');
  symlinkSync(real, alias, process.platform === 'win32' ? 'junction' : 'dir');
  return import('../src/resolve.mjs').then(({ resolveRuntimePath }) => {
    const result = resolveRuntimePath({ env: { ALEF_BIN: join(alias, 'alef') } });
    assert.ok(isAbsolute(result)); assert.equal(readFileSync(result, 'utf8'), 'aliased');
  });
});
