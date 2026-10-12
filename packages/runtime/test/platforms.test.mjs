// SPDX-License-Identifier: MIT OR Apache-2.0
import test from 'node:test';
import assert from 'node:assert/strict';
import { readFileSync } from 'node:fs';
import { PLATFORMS, PLATFORM_VERSION_RELATION, packageName } from '../src/platforms.mjs';

test('platform_table_pins_the_four_supported_configurations_and_ci_binary_names', () => {
  assert.deepEqual(PLATFORMS.map(entry => [entry.platform, entry.arch, entry.suffix, entry.binary, entry.ciName]), [
    ['win32', 'x64', 'win32-x64', 'alef.exe', 'windows-x64'],
    ['darwin', 'arm64', 'darwin-arm64', 'alef', 'macos-arm64'],
    ['darwin', 'x64', 'darwin-x64', 'alef', 'macos-x64'],
    ['linux', 'x64', 'linux-x64', 'alef', 'linux-x64'],
  ]);
  const ci = readFileSync(new URL('../../../.github/workflows/ci.yml', import.meta.url), 'utf8');
  const rows = [...ci.matchAll(/- name: (windows-x64|macos-arm64|macos-x64|linux-x64)\r?\n\s+os: [^\r\n]+\r?\n\s+target: [^\r\n]+\r?\n\s+bin: ([^\s]+)/g)];
  assert.equal(rows.length, 4);
  assert.deepEqual(rows.map(row => [row[1], row[2]]), PLATFORMS.map(entry => [entry.ciName, entry.binary]));
});

test('private_esm_meta_package_uses_the_single_shared_dependency_version_relation', () => {
  const manifest = JSON.parse(readFileSync(new URL('../package.json', import.meta.url)));
  assert.equal(manifest.name, '@alef-tron/runtime');
  assert.equal(manifest.version, '0.1.0');
  assert.equal(manifest.private, true);
  assert.equal(manifest.type, 'module');
  assert.equal(manifest.engines.node, '>=22.12.0');
  assert.equal(manifest.license, 'MIT OR Apache-2.0');
  assert.deepEqual(manifest.exports, { '.': './src/index.mjs' });
  assert.deepEqual(manifest.bin, { alef: './bin/alef.mjs' });
  assert.deepEqual(manifest.optionalDependencies, Object.fromEntries(PLATFORMS.map(entry => [packageName(entry), PLATFORM_VERSION_RELATION])));
  assert.ok(manifest.files.includes('src') && manifest.files.includes('bin'));
  assert.equal(manifest.scripts, undefined);
});
