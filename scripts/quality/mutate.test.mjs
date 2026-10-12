import assert from 'node:assert/strict';
import { mkdirSync, mkdtempSync, readFileSync, existsSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { spawnSync } from 'node:child_process';
import test from 'node:test';
import { loadList, run, runMutation, selectShard, validateList } from './mutate.mjs';

function setup(files) {
  const root = mkdtempSync(path.join(tmpdir(), 'mutate-'));
  for (const [name, content] of Object.entries(files)) {
    const file = path.join(root, name);
    mkdirSync(path.dirname(file), { recursive: true });
    writeFileSync(file, content);
  }
  return root;
}

const TARGET = 'backend/crates/demo/src/lib.rs';
const GOOD = 'pub fn add(a: i32, b: i32) -> i32 {\n    a + b\n}\n';
const MARKER = 'a.wrapping_add(b)';

// Fake test command: exits 101 (failure) when the target file carries the mutation, else 0.
function fakeScript(relativeTarget, extra = '') {
  return `
    import { readFileSync, writeFileSync } from 'node:fs';
    const text = readFileSync('${relativeTarget.split('\\').join('\\\\')}', 'utf8');
    ${extra}
    process.exit(text.includes('MUTATED') ? 101 : 0);
  `;
}

function mutation(overrides = {}) {
  return { name: 'the addition overflows', file: TARGET, from: 'a + b', to: `a ${MARKER} // MUTATED`, package: 'demo', ...overrides };
}

function fakeSpawnOf(root) {
  const script = path.join(root, 'fake-test.mjs');
  writeFileSync(script, fakeScript(TARGET));
  return spawnSync;
}

test('a mutation whose tests fail is caught and the run exits zero', () => {
  const root = setup({ [TARGET]: GOOD, 'scripts/quality/mutations/list.json': JSON.stringify([mutation()]) });
  try {
    const result = run(['list.json'], { env: { MUTATE_COMMAND: JSON.stringify(['node', 'fake-test.mjs']) }, root, spawn: fakeSpawnOf(root), log: () => {} });
    assert.deepEqual(result.rows.map(row => row[0]), ['CAUGHT']);
    assert.equal(result.code, 0);
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test('a mutation whose tests pass is escaped and the run exits one', () => {
  const root = setup({ [TARGET]: GOOD, 'scripts/quality/mutations/list.json': JSON.stringify([mutation()]) });
  try {
    // The fake only fails on the marker, so a replacement without it leaves the tests green.
    const list = [mutation({ to: 'a - b' })];
    writeFileSync(path.join(root, 'scripts/quality/mutations/list.json'), JSON.stringify(list));
    const result = run(['list.json'], { env: { MUTATE_COMMAND: JSON.stringify(['node', 'fake-test.mjs']) }, root, spawn: fakeSpawnOf(root), log: () => {} });
    assert.deepEqual(result.rows.map(row => row[0]), ['ESCAPED']);
    assert.equal(result.code, 1);
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test('an anchor that is not in the file is anchor_missing and the command is never run', () => {
  const root = setup({ [TARGET]: GOOD, 'scripts/quality/mutations/list.json': JSON.stringify([mutation({ from: 'no such anchor' })]) });
  try {
    const sentinel = path.join(root, 'sentinel.txt');
    const script = path.join(root, 'fake-test.mjs');
    writeFileSync(script, `${fakeScript(TARGET, `writeFileSync(${JSON.stringify(sentinel)}, 'x');`)}`);
    const result = run(['list.json'], { env: { MUTATE_COMMAND: JSON.stringify(['node', 'fake-test.mjs']) }, root, spawn: spawnSync, log: () => {} });
    assert.deepEqual(result.rows.map(row => row[0]), ['ANCHOR_MISSING']);
    assert.equal(result.code, 1);
    assert.equal(existsSync(sentinel), false);
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test('an anchor that occurs twice is anchor_ambiguous and the command is never run', () => {
  const twice = `${GOOD}pub fn sum(a: i32, b: i32) -> i32 {\n    a + b\n}\n`;
  const root = setup({ [TARGET]: twice, 'scripts/quality/mutations/list.json': JSON.stringify([mutation()]) });
  try {
    const sentinel = path.join(root, 'sentinel.txt');
    const script = path.join(root, 'fake-test.mjs');
    writeFileSync(script, `${fakeScript(TARGET, `writeFileSync(${JSON.stringify(sentinel)}, 'x');`)}`);
    const result = run(['list.json'], { env: { MUTATE_COMMAND: JSON.stringify(['node', 'fake-test.mjs']) }, root, spawn: spawnSync, log: () => {} });
    assert.deepEqual(result.rows.map(row => row[0]), ['ANCHOR_AMBIGUOUS']);
    assert.equal(result.code, 1);
    assert.equal(existsSync(sentinel), false);
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test('a command that reports a compile error is invalid even though it exits 101', () => {
  const root = setup({ [TARGET]: GOOD, 'scripts/quality/mutations/list.json': JSON.stringify([mutation({ to: 'this is not rust // MUTATED' })]) });
  try {
    const script = path.join(root, 'fake-test.mjs');
    writeFileSync(script, `
      console.error('error[E0308]: mismatched types');
      process.exit(101);
    `);
    const result = run(['list.json'], { env: { MUTATE_COMMAND: JSON.stringify(['node', 'fake-test.mjs']) }, root, spawn: spawnSync, log: () => {} });
    assert.deepEqual(result.rows.map(row => row[0]), ['INVALID']);
    assert.equal(result.code, 1);
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test('the target file is restored byte for byte after caught, escaped and invalid runs', () => {
  for (const fake of ['a + b // MUTATED', 'a - b', 'this is not rust // MUTATED']) {
    const root = setup({ [TARGET]: GOOD, 'scripts/quality/mutations/list.json': JSON.stringify([mutation({ to: fake })]) });
    try {
      run(['list.json'], { env: { MUTATE_COMMAND: JSON.stringify(['node', 'fake-test.mjs']) }, root, spawn: fakeSpawnOf(root), log: () => {} });
      assert.equal(readFileSync(path.join(root, TARGET), 'utf8'), GOOD);
    } finally { rmSync(root, { recursive: true, force: true }); }
  }
});

test('the file is restored even when the spawned command throws synchronously', () => {
  const root = setup({ [TARGET]: GOOD, 'scripts/quality/mutations/list.json': JSON.stringify([mutation()]) });
  try {
    const result = run(['list.json'], { env: {}, root, spawn: () => { throw new Error('spawn exploded'); }, log: () => {} });
    assert.deepEqual(result.rows.map(row => row[0]), ['INVALID']);
    assert.match(result.rows[0][3], /spawn exploded/);
    assert.equal(result.code, 1);
    assert.equal(readFileSync(path.join(root, TARGET), 'utf8'), GOOD);
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test('a timed-out command with no running test in the output is a timeout and the file is restored', () => {
  const root = setup({ [TARGET]: GOOD, 'scripts/quality/mutations/list.json': JSON.stringify([mutation()]) });
  try {
    const spawn = () => ({ status: null, stdout: '', stderr: '', error: { code: 'ETIMEDOUT' } });
    const result = run(['list.json'], { env: {}, root, spawn, log: () => {} });
    assert.deepEqual(result.rows.map(row => row[0]), ['TIMEOUT']);
    assert.equal(result.code, 1);
    assert.equal(readFileSync(path.join(root, TARGET), 'utf8'), GOOD);
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test('a hang while a test is running counts as caught', () => {
  const root = setup({ [TARGET]: GOOD, 'scripts/quality/mutations/list.json': JSON.stringify([mutation()]) });
  try {
    const spawn = () => ({ status: null, stdout: '\ntest the_thing ... ', stderr: '', error: { code: 'ETIMEDOUT' } });
    const result = run(['list.json'], { env: {}, root, spawn, log: () => {} });
    assert.deepEqual(result.rows.map(row => row[0]), ['CAUGHT']);
    assert.equal(result.code, 0);
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test('the shards of three over seven mutations partition the list exactly', () => {
  const list = Array.from({ length: 7 }, (_, i) => mutation({ name: `mutation number ${i}` }));
  assert.deepEqual(validateList(list), list);
  const shards = [1, 2, 3].map(i => selectShard(list, i, 3).map(m => m.name));
  assert.deepEqual(shards[0], ['mutation number 0', 'mutation number 3', 'mutation number 6']);
  assert.deepEqual(shards[1], ['mutation number 1', 'mutation number 4']);
  assert.deepEqual(shards[2], ['mutation number 2', 'mutation number 5']);
  assert.deepEqual([...shards[0], ...shards[1], ...shards[2]].sort(), list.map(m => m.name).sort());
});

test('a filter keeps only the matching mutations and never touches the other file', () => {
  const other = 'backend/crates/demo/src/other.rs';
  const root = setup({
    [TARGET]: GOOD,
    [other]: GOOD,
    'scripts/quality/mutations/list.json': JSON.stringify([
      mutation({ name: 'alpha breaks the addition' }),
      mutation({ name: 'beta breaks the addition', file: other, from: 'a + b', to: 'a - b // MUTATED' }),
    ]),
  });
  try {
    const script = path.join(root, 'fake-test.mjs');
    writeFileSync(script, fakeScript(TARGET));
    const result = run(['list.json', '--filter', '^alpha'], { env: { MUTATE_COMMAND: JSON.stringify(['node', 'fake-test.mjs']) }, root, spawn: spawnSync, log: () => {} });
    assert.deepEqual(result.rows.map(row => row[1]), ['alpha breaks the addition']);
    assert.equal(result.code, 0);
    assert.equal(readFileSync(path.join(root, other), 'utf8'), GOOD);
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test('a file with Windows line endings keeps them across a found mutation and its restore', () => {
  const crlf = GOOD.replaceAll('\n', '\r\n');
  const root = setup({ [TARGET]: crlf, 'scripts/quality/mutations/list.json': JSON.stringify([mutation()]) });
  try {
    const result = run(['list.json'], { env: { MUTATE_COMMAND: JSON.stringify(['node', 'fake-test.mjs']) }, root, spawn: fakeSpawnOf(root), log: () => {} });
    assert.deepEqual(result.rows.map(row => row[0]), ['CAUGHT']);
    assert.equal(readFileSync(path.join(root, TARGET), 'utf8'), crlf);
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test('a list entry of the wrong shape is refused with the mutation name and the field', () => {
  assert.throws(() => validateList([{ ...mutation(), to: 7 }]), /mutation\[0\].*"to" must be a string/);
  assert.throws(() => validateList([{ ...mutation(), name: 'the features is a number', features: 3 }]), /"features" must be a string/);
  assert.throws(() => validateList([{ ...mutation(), surprise: true }]), /unknown key/);
  assert.throws(() => validateList([]), /empty/);
  assert.throws(() => validateList({ not: 'an array' }), /must be a JSON array/);
});

test('a named list loads from the mutations folder and a path list from the repository root', () => {
  const root = setup({ 'scripts/quality/mutations/named.json': JSON.stringify([mutation()]) });
  try {
    assert.deepEqual(loadList('named.json', root), [mutation()]);
    assert.deepEqual(loadList('named', root), [mutation()]);
  } finally { rmSync(root, { recursive: true, force: true }); }
});

test('a direct runMutation classifies an escaped mutation without touching the process exit code', () => {
  const root = setup({ [TARGET]: GOOD });
  try {
    const spawn = () => ({ status: 0, stdout: '', stderr: '' });
    const outcome = runMutation(mutation({ to: 'a - b // MUTATED' }), { env: {}, root, spawn });
    assert.equal(outcome.outcome, 'ESCAPED');
  } finally { rmSync(root, { recursive: true, force: true }); }
});
