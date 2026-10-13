// SPDX-License-Identifier: MIT OR Apache-2.0
import assert from 'node:assert/strict';
import test from 'node:test';
import { parseCrates } from './test-crates.mjs';

test('a_comma_list_gives_the_crates_in_order_without_duplicates', () => {
  assert.deepEqual(parseCrates('alef,alef-runtime,alef'), ['alef', 'alef-runtime']);
});

test('spaces_and_empty_items_are_ignored', () => {
  assert.deepEqual(parseCrates(' alef , ,alef-core '), ['alef', 'alef-core']);
});

test('an_empty_or_missing_list_gives_nothing', () => {
  assert.deepEqual(parseCrates(''), []);
  assert.deepEqual(parseCrates(undefined), []);
});

test('names_that_could_inject_arguments_are_dropped', () => {
  assert.deepEqual(parseCrates('alef,--manifest-path,$(x),a;b'), ['alef']);
});
