import assert from 'node:assert/strict';
import { mkdirSync, mkdtempSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { checkStructure, MAX_ENTRIES, MAX_LINES } from './check-structure.mjs';

function tree(files) {
  const root = mkdtempSync(path.join(tmpdir(), 'structure-'));
  for (const [name, content] of Object.entries(files)) {
    const file = path.join(root, name);
    mkdirSync(path.dirname(file), { recursive: true });
    writeFileSync(file, content);
  }
  return root;
}
const lines = (count, end = '\n') => `${Array.from({ length: count }, () => 'x').join(end)}${end}`;
function violationsOf(files, options = { sources: ['src'] }) {
  const root = tree(files);
  try { return checkStructure(root, options); } finally { rmSync(root, { recursive: true, force: true }); }
}

test('the limits of the project are seven entries per folder and a thousand lines per file', () => {
  assert.equal(MAX_ENTRIES, 7);
  assert.equal(MAX_LINES, 1000);
});

test('a folder with seven entries passes and a folder with eight is reported by name', () => {
  const seven = Object.fromEntries(Array.from({ length: 7 }, (_, i) => [`src/f${i}.rs`, '']));
  assert.deepEqual(violationsOf(seven), []);
  const eight = Object.fromEntries(Array.from({ length: 8 }, (_, i) => [`src/f${i}.rs`, '']));
  assert.deepEqual(violationsOf(eight), ['src/: 8 entries (max 7)']);
});

test('folders count as entries and the limit applies at every level', () => {
  const files = Object.fromEntries(Array.from({ length: 8 }, (_, i) => [`src/deep/d${i}/mod.rs`, '']));
  assert.deepEqual(violationsOf(files), ['src/deep/: 8 entries (max 7)']);
});

test('a file of a thousand lines passes and one more line is reported with its count', () => {
  assert.deepEqual(violationsOf({ 'src/a.rs': lines(1000) }), []);
  assert.deepEqual(violationsOf({ 'src/a.rs': lines(1001) }), ['src/a.rs: 1001 lines (max 1000)']);
});

test('line ends of Windows count like those of Unix and a missing final newline counts the last line', () => {
  assert.deepEqual(violationsOf({ 'src/a.rs': lines(1000, '\r\n') }), []);
  assert.deepEqual(violationsOf({ 'src/a.rs': lines(1001, '\r\n') }), ['src/a.rs: 1001 lines (max 1000)']);
  assert.deepEqual(violationsOf({ 'src/a.rs': lines(1000).slice(0, -1) }), []);
  assert.deepEqual(violationsOf({ 'src/a.rs': lines(1001).slice(0, -1) }), ['src/a.rs: 1001 lines (max 1000)']);
});

test('only code files are measured by lines and generated or vendored folders are skipped', () => {
  assert.deepEqual(violationsOf({ 'src/notes.md': lines(5000), 'src/data.json': lines(5000) }), []);
  for (const skipped of ['node_modules', 'dist', 'target']) {
    const files = Object.fromEntries(Array.from({ length: 9 }, (_, i) => [`src/${skipped}/f${i}.rs`, lines(5000)]));
    assert.deepEqual(violationsOf(files), [], skipped);
  }
});

test('roots that do not exist are skipped and the limits can be narrowed by the caller', () => {
  assert.deepEqual(violationsOf({ 'src/a.rs': '' }, { sources: ['missing', 'src'] }), []);
  assert.deepEqual(violationsOf({ 'src/a.rs': lines(3) }, { sources: ['src'], maxLines: 2 }), ['src/a.rs: 3 lines (max 2)']);
  assert.deepEqual(violationsOf({ 'src/a.rs': '', 'src/b.rs': '' }, { sources: ['src'], maxEntries: 1 }), ['src/: 2 entries (max 1)']);
});

test('the repository itself keeps the limits', () => {
  const root = path.resolve(import.meta.dirname, '..', '..');
  assert.deepEqual(checkStructure(root), []);
});
