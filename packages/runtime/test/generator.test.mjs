// SPDX-License-Identifier: MIT OR Apache-2.0
import test from 'node:test';
import assert from 'node:assert/strict';
import { mkdirSync, readFileSync, readdirSync, statSync, symlinkSync, writeFileSync } from 'node:fs';
import { join } from 'node:path';
import { PLATFORMS, packageName } from '../src/platforms.mjs';
import { generate, root, temporary } from './helpers.mjs';

for (const entry of PLATFORMS) {
  test(`generates_${entry.suffix.replaceAll('-', '_')}_metadata_exact_bytes_licenses_and_executable_mode`, t => {
    const directory = temporary(t); const input = join(directory, 'source binary');
    const output = join(directory, 'output'); const bytes = Buffer.from([0, 255, 13, 10, 42]);
    writeFileSync(input, bytes, { mode: 0o600 });
    const result = generate(entry.suffix, input, output, '0.1.0-rc.1+build.7');
    assert.equal(result.status, 0, result.stderr);
    const manifest = JSON.parse(readFileSync(join(output, 'package.json')));
    assert.equal(manifest.name, packageName(entry));
    assert.equal(manifest.version, '0.1.0-rc.1+build.7');
    assert.deepEqual(manifest.os, [entry.platform]); assert.deepEqual(manifest.cpu, [entry.arch]);
    assert.deepEqual(manifest.bin, { alef: `./bin/${entry.binary}` });
    assert.equal(manifest.license, 'MIT OR Apache-2.0');
    assert.deepEqual(manifest.files, ['bin', 'README.md', 'LICENSE-MIT', 'LICENSE-APACHE']);
    assert.deepEqual(readdirSync(join(output, 'bin')), [entry.binary]);
    const binary = join(output, 'bin', entry.binary);
    assert.deepEqual(readFileSync(binary), bytes);
    if (process.platform !== 'win32' && entry.platform !== 'win32') assert.equal(statSync(binary).mode & 0o777, 0o755);
    const readme = readFileSync(join(output, 'README.md'), 'utf8');
    assert.ok(readme.includes(manifest.name) && readme.includes(manifest.version));
    for (const license of ['LICENSE-MIT', 'LICENSE-APACHE']) {
      assert.deepEqual(readFileSync(join(output, license)), readFileSync(join(root, '..', '..', license)));
    }
    const retry = generate(entry.suffix, input, output, '0.1.0');
    assert.notEqual(retry.status, 0); assert.match(retry.stderr, /not empty.*refusing to overwrite/);
    assert.deepEqual(readFileSync(binary), bytes); assert.equal(JSON.parse(readFileSync(join(output, 'package.json'))).version, manifest.version);
  });
}

test('generator_rejects_missing_or_non_file_input_before_creating_output', t => {
  const directory = temporary(t); const output = join(directory, 'out');
  for (const input of [join(directory, 'missing'), directory]) {
    const result = generate('linux-x64', input, output, '0.1.0');
    assert.notEqual(result.status, 0); assert.match(result.stderr, /Input binary.*(?:missing|not a regular file)/);
    assert.equal(readdirSync(directory).length, 0);
  }
});

test('generator_accepts_an_empty_output_and_refuses_nonempty_or_file_output_without_changing_contents', t => {
  const directory = temporary(t); const input = join(directory, 'input'); writeFileSync(input, 'original');
  const empty = join(directory, 'empty'); mkdirSync(empty);
  assert.equal(generate('linux-x64', input, empty, '0.1.0').status, 0);
  const full = join(directory, 'full'); mkdirSync(full); writeFileSync(join(full, '.hidden'), 'keep');
  assert.match(generate('linux-x64', input, full, '0.1.0').stderr, /not empty/);
  assert.deepEqual(readdirSync(full), ['.hidden']); assert.equal(readFileSync(join(full, '.hidden'), 'utf8'), 'keep');
  assert.notEqual(generate('linux-x64', input, input, '0.1.0').status, 0);
  assert.equal(readFileSync(input, 'utf8'), 'original');
});

test('generator_validates_argument_count_platform_and_semver_without_writing_output', t => {
  const directory = temporary(t); const input = join(directory, 'input'); writeFileSync(input, 'bytes');
  const output = join(directory, 'out');
  for (const args of [[], ['linux-x64', input, output], ['linux-x64', input, output, '0.1.0', 'extra'],
    ['../linux-x64', input, output, '0.1.0'], ['linux-x64', input, output, ''],
    ...['v0.1.0', '01.1.0', '0.1', '0.1.0-01', '0.1.0+'].map(version => ['linux-x64', input, output, version])]) {
    const result = generate(...args); assert.notEqual(result.status, 0); assert.match(result.stderr, /Usage|Unsupported|Invalid SemVer/);
  }
  assert.deepEqual(readdirSync(directory), ['input']);
});

test('generator_follows_symlinked_input_and_output_parents_and_refuses_a_nonempty_alias_of_source', t => {
  const directory = temporary(t); const real = join(directory, 'real'); mkdirSync(real);
  const alias = join(directory, 'alias'); symlinkSync(real, alias, process.platform === 'win32' ? 'junction' : 'dir');
  writeFileSync(join(real, 'input'), 'alias bytes');
  const result = generate('linux-x64', join(alias, 'input'), join(alias, 'out'), '0.1.0');
  assert.equal(result.status, 0, result.stderr);
  assert.equal(readFileSync(join(real, 'out', 'bin', 'alef'), 'utf8'), 'alias bytes');
  const refusal = generate('linux-x64', join(real, 'input'), alias, '0.1.0');
  assert.notEqual(refusal.status, 0); assert.match(refusal.stderr, /not empty/);
  assert.equal(readFileSync(join(real, 'input'), 'utf8'), 'alias bytes');
});
