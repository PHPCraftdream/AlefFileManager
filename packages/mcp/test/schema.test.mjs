// SPDX-License-Identifier: MIT OR Apache-2.0
import './regression/schema.mjs';
import assert from 'node:assert/strict';
import test from 'node:test';
import { validate } from '../src/schema.ts';
test('The schema subset validates properties, required fields, items and additional properties.', () => {
  const schema = { type: 'object', required: ['items'], properties: { items: { type: 'array', items: { type: 'integer', minimum: 1, maximum: 3 } } }, additionalProperties: false };
  assert.deepEqual(validate(schema, { items: [1, 3] }), []);
  for (const value of [{}, { items: [0] }, { items: [4] }, { items: [1.5] }, { items: [1], extra: true }, []]) assert.ok(validate(schema, value).length);
  assert.deepEqual(validate({ additionalProperties: { type: 'boolean' } }, { yes: true }), []);
  assert.ok(validate({ additionalProperties: { type: 'boolean' } }, { yes: 1 }).length);
});
test('The schema subset validates strings, structural enum and const values and union types.', () => {
  const schema = { type: 'string', minLength: 1, maxLength: 2, pattern: '^a' };
  assert.deepEqual(validate(schema, 'a'), []);
  for (const value of ['', 'abc', 'b']) assert.ok(validate(schema, value).length);
  assert.deepEqual(validate({ minLength: 1, maxLength: 1 }, '😀'), []);
  assert.deepEqual(validate({ enum: [{ x: 1, y: 2 }] }, { y: 2, x: 1 }), []);
  assert.ok(validate({ const: [1] }, [2]).length);
  assert.deepEqual(validate({ type: ['null', 'boolean'] }, null), []);
  assert.deepEqual(validate({ type: ['null', 'boolean'] }, true), []);
  assert.ok(validate({ type: 'number' }, Infinity).length);
});
test('The schema subset validates combinators and local definitions without remote references.', () => {
  assert.deepEqual(validate({ allOf: [{ minimum: 1 }, { maximum: 2 }] }, 2), []);
  assert.ok(validate({ allOf: [{ minimum: 1 }] }, 0).length);
  assert.deepEqual(validate({ anyOf: [{ const: 1 }, { const: 2 }] }, 2), []);
  assert.ok(validate({ anyOf: [{ const: 1 }] }, 2).length);
  assert.ok(validate({ oneOf: [{ type: 'number' }, { type: 'integer' }] }, 1).length);
  assert.deepEqual(validate({ oneOf: [{ const: 1 }, { const: 2 }] }, 1), []);
  assert.ok(validate({ not: { const: 1 } }, 1).length);
  assert.deepEqual(validate({ $defs: { 'a/b': { type: 'string' } }, $ref: '#/$defs/a~1b' }, 'ok'), []);
  assert.throws(() => validate({ $ref: 'https://example.test/schema' }, 1), /local/);
  assert.throws(() => validate({ $ref: '#/$defs/missing' }, 1), /Unresolved/);
  assert.ok(validate({ $defs: { recursive: { $ref: '#/$defs/recursive' } }, $ref: '#/$defs/recursive' }, 1).length);
  assert.deepEqual(validate(true, 1), []); assert.ok(validate(false, 1).length);
});
