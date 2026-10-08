// SPDX-License-Identifier: MIT OR Apache-2.0
import assert from 'node:assert/strict';
import test from 'node:test';
import { validate } from '../../src/schema.ts';

test('Multi-member enums accept every individual member, not their intersection.', () => {
  const members = ['one', 'two', { nested: [1, 2] }];
  for (const member of members) assert.deepEqual(validate({ enum: members }, structuredClone(member)), []);
  assert.ok(validate({ enum: members }, 'three').length);
});

test('Const and enum array equality includes length, including empty arrays and nested prefixes.', () => {
  for (const expected of [[], [1], [[1]]]) {
    for (const schema of [{ const: expected }, { enum: [expected] }]) {
      assert.deepEqual(validate(schema, structuredClone(expected)), []);
      assert.ok(validate(schema, [...expected, 2]).length);
      if (expected.length) assert.ok(validate(schema, []).length);
    }
  }
  assert.ok(validate({ const: [[1]] }, [[1, 2]]).length);
  assert.ok(validate({ enum: [[[1]]] }, [[1, 2]]).length);
});
