// SPDX-License-Identifier: MIT OR Apache-2.0
import assert from 'node:assert/strict';
import { spawnSync } from 'node:child_process';
import { mkdtemp, readFile, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';

const scanner = fileURLToPath(new URL('./check-no-hooks.mjs', import.meta.url));
// Keep the oracle independent of the scanner's marker list.
const markers = ['e2e.dropFiles', 'e2e.once', 'e2e.flood', 'ALEF_E2E', 'ALEF_SPIKE_ORIGIN'];
const encodings = [['UTF8', 'utf8'], ['UTF16LE', 'utf16le']];

test('ci_has_an_independent_linux_release_build_without_hooks', { timeout: 5000 }, async () => {
  // A Windows checkout may turn the line ends into CRLF.
  const yaml = (await readFile(new URL('../../.github/workflows/ci.yml', import.meta.url), 'utf8')).replace(/\r\n/g, '\n');
  // Only inspect this step in the existing YAML layout, not other jobs or settings.
  const steps = yaml.split(/^      - /m);
  const release = steps.filter(step => step.startsWith('name: Release build without test hooks (Linux)\n'));
  assert.equal(release.length, 1, 'expected one independent release step');
  // The planner may add its own condition (a docs-only push builds nothing); the step stays Linux-only.
  assert.match(release[0], /^        if: runner\.os == 'Linux'( && .+)?\s*$/m);
  const run = release[0].match(/^        run: \|\r?\n((?: {10}[^\r\n]*\r?\n|\r?\n)*)/m);
  assert.ok(run, 'expected a release command block');
  const commands = run[1].split(/\r?\n/).map(line => line.trim()).filter(line => line && !line.startsWith('#'));
  assert.equal(commands.length, 2, 'release build and scanner must be independent of the feature build');
  assert.doesNotMatch(commands.join('\n'), /--features\b|--all-features\b|e2e/i);
  assert.match(commands[0], /^node scripts\/cargo\.mjs build\b/);
  assert.match(commands[0], /(?:^|\s)-p alef(?:\s|$)/);
  assert.match(commands[0], /(?:^|\s)--release(?:\s|$)/);
  assert.equal(commands[1], 'node scripts/supply/check-no-hooks.mjs backend/target/release/alef');
});

function run_scanner(args) {
  const result = spawnSync(process.execPath, [scanner, ...args], {
    encoding: 'utf8', timeout: 5000, maxBuffer: 16 * 1024,
  });
  assert.equal(result.error, undefined);
  assert.equal(result.signal, null);
  return result;
}

async function with_fake_binary(bytes, check) {
  const directory = await mkdtemp(path.join(tmpdir(), 'alef-no-hooks-'));
  try {
    const binary = path.join(directory, 'fake.bin');
    await writeFile(binary, bytes);
    await check(binary, directory);
  } finally {
    await rm(directory, { recursive: true, force: true });
  }
}

function assert_found(result, marker, label) {
  assert.equal(result.status, 1);
  assert.ok(result.stderr.split(/\r?\n/).includes(`Found: ${marker} (${label})`), result.stderr);
}

test('a_clean_binary_passes_and_reports_the_looked_for_list_and_byte_size', async () => {
  const bytes = Buffer.from([0xff, 0xfe, 0, 0x80, 0x81, 0x42, 0]);
  await with_fake_binary(bytes, binary => {
    const result = run_scanner([binary]);
    assert.equal(result.status, 0);
    assert.equal(result.stderr, '');
    assert.ok(result.stdout.includes(`Looked for: ${markers.join(', ')}`));
    assert.ok(result.stdout.includes(`Byte size: ${bytes.length}`));
  });
});

for (const [label, encoding] of encodings) {
  for (const marker of markers) {
    test(`a_binary_with_only_${marker.replaceAll('.', '_')}_in_${label}_fails_and_reports_the_marker`, async () => {
      // The odd offset also exercises unaligned UTF16LE bytes.
      const bytes = Buffer.concat([Buffer.from([0xff]), Buffer.from(marker, encoding), Buffer.from([0x80])]);
      await with_fake_binary(bytes, binary => {
        const result = run_scanner([binary]);
        assert_found(result, marker, label);
        assert.equal(result.stderr.trim().split(/\r?\n/).length, 1);
        assert.ok(result.stdout.includes(`Byte size: ${bytes.length}`));
      });
    });
  }
}

test('a_binary_with_multiple_markers_reports_every_marker_in_both_encodings', async () => {
  const bytes = Buffer.concat(encodings.flatMap(([, encoding]) =>
    markers.map(marker => Buffer.concat([Buffer.from([0xff]), Buffer.from(marker, encoding)]))));
  await with_fake_binary(bytes, binary => {
    const result = run_scanner([binary]);
    for (const [label] of encodings) {
      for (const marker of markers) assert_found(result, marker, label);
    }
    assert.equal(result.stderr.trim().split(/\r?\n/).length, markers.length * encodings.length);
  });
});

test('invalid_arguments_and_missing_or_unreadable_paths_fail_with_bounded_diagnostics', async () => {
  await with_fake_binary(Buffer.alloc(0), (binary, directory) => {
    const invalid_args = [[], [binary, binary], [''], ['--help'], ['x'.repeat(10000)],
      [path.join(directory, 'missing.bin')], [directory]];
    for (const args of invalid_args) {
      const result = run_scanner(args);
      assert.equal(result.status, 1);
      assert.equal(result.stdout, '');
      assert.ok(result.stderr.length > 0 && result.stderr.length <= 256, result.stderr);
      assert.match(result.stderr, /Usage:|Cannot read binary:/);
    }
  });
});
