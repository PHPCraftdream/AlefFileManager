// SPDX-License-Identifier: MIT OR Apache-2.0
import assert from 'node:assert/strict';
import test from 'node:test';
import { readFileSync } from 'node:fs';
import { verify } from '../src/verify.mjs';

const lines = readFileSync(new URL('../../../tests/fixtures/package-vectors.txt', import.meta.url), 'utf8').split('\n').filter(Boolean);

test('the_shared_vectors_are_present_and_well_formed', () => {
  assert.ok(lines.length >= 80);
  const names = new Set();
  for (const line of lines) {
    const [name, expect, hex] = line.split('\t');
    assert.match(expect, /^(reject|unsigned|signed:[0-9a-f]{64})$/);
    assert.match(hex, /^([0-9a-f]{2})*$/);
    assert.ok(!names.has(name)); names.add(name);
  }
});

for (const line of lines) {
  const [name, expect, hex] = line.split('\t');
  test(`shared_vector_${name.replaceAll('-', '_')}`, { timeout: 20000 }, async () => {
    const bytes = Buffer.from(hex, 'hex');
    if (expect === 'reject') { await assert.rejects(verify(bytes)); return; }
    const info = await verify(bytes);
    if (expect === 'unsigned') assert.equal(info.status, 'Unsigned');
    else { assert.equal(info.status, 'Signed'); assert.equal(`signed:${info.fingerprint}`, expect); }
  });
}
