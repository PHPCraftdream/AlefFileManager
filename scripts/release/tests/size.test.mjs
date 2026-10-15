// node --test scripts/release/tests/size.test.mjs: byte counts, summary row and JSON output of size.mjs.
import assert from 'node:assert/strict';
import { mkdtemp, rm, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { formatSummary, sizeReport, writeJson } from '../size.mjs';

test('sizeReport_returns_the_exact_byte_counts_of_both_files', async () => {
  const dir = await mkdtemp(path.join(tmpdir(), 'alef-size-'));
  try {
    const binaryPath = path.join(dir, 'alef');
    const archivePath = path.join(dir, 'alef.tar.gz');
    await writeFile(binaryPath, Buffer.alloc(11, 'a'));
    await writeFile(archivePath, Buffer.alloc(2048));
    const report = sizeReport({ binaryPath, archivePath, profile: 'dist' });
    assert.equal(report.binaryBytes, 11);
    assert.equal(report.archiveBytes, 2048);
    assert.equal(report.profile, 'dist');
  } finally {
    await rm(dir, { recursive: true, force: true });
  }
});

test('formatSummary_produces_one_table_row_with_mib_sizes', () => {
  assert.equal(
    formatSummary({ binaryBytes: 1048576, archiveBytes: 2097152, profile: 'dist' }, 'linux-x64'),
    '| linux-x64 | 1048576 | 1.0 MiB | 2097152 | 2.0 MiB | dist |',
  );
});

test('writeJson_emits_parseable_json_with_the_three_fields', () => {
  const parsed = JSON.parse(writeJson({ binaryBytes: 7, archiveBytes: 9, profile: 'dist', platform: 'linux-x64' }));
  assert.equal(parsed.binaryBytes, 7);
  assert.equal(parsed.archiveBytes, 9);
  assert.equal(parsed.profile, 'dist');
  assert.equal(parsed.platform, 'linux-x64');
});
