// SPDX-License-Identifier: MIT OR Apache-2.0
import assert from 'node:assert/strict';
import test from 'node:test';
import { createHash, verify as cryptoVerify } from 'node:crypto';
import fs, { mkdtemp, mkdir, writeFile, readFile, rm, open, symlink } from 'node:fs/promises';
import { syncBuiltinESMExports } from 'node:module';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { createKey, loadKey } from '../src/keys.mjs';
import { pack } from '../src/pack.mjs';
import { verify } from '../src/verify.mjs';
import { readZip, writeZip } from '../src/zip.mjs';

const bounded = { timeout: 20000 };
const hash = bytes => createHash('sha256').update(bytes).digest('hex');
async function scratch(run) {
  const root = await mkdtemp(path.join(tmpdir(), 'alef-pack-')); const dir = path.join(root, 'app'); const home = path.join(root, 'home');
  try {
    await mkdir(dir); await writeFile(path.join(dir, 'alef.ktav'), 'app: example\n');
    await run({ root, dir, home });
  } finally { await rm(root, { recursive: true, force: true }); }
}
test('signed_packages_with_default_and_explicit_keys_roundtrip_exact_canonical_deterministic_zip_bytes', bounded, () => scratch(async ({ dir, home }) => {
  const key = await createKey(undefined, { home });
  for (const [name, data] of [['@asset', 'x'], ['+asset', 'a'.repeat(4096)], ['index.html', 'index.html']]) await writeFile(path.join(dir, name), data);
  const result = await pack(dir, { home });
  assert.equal(result.out, `${dir}.alef`); assert.equal(result.status, 'Signed'); assert.equal(result.count, 6);
  assert.equal(result.fingerprint, key.fingerprint);
  const bytes = await readFile(result.out); const info = await verify(bytes);
  assert.equal(info.fingerprint, key.fingerprint); assert.deepEqual(info.key, key.key);
  const archive = await readZip(bytes);
  const entries = [];
  for (const entry of archive.entries) entries.push({ name: entry.name, data: await entry.read() });
  assert.deepEqual(bytes, writeZip(entries));
  assert.deepEqual(entries.map(entry => entry.name), ['+asset', '@asset', 'META-INF/hashes', 'META-INF/signature', 'alef.ktav', 'index.html']);
  // Inspect the wire headers independently of the ZIP reader and writer.
  const end = bytes.length - 22;
  assert.equal(bytes.readUInt32LE(end), 0x06054b50);
  let central = bytes.readUInt32LE(end + 16); const methods = new Map();
  for (const entry of entries) {
    const name = Buffer.from(entry.name); const local = bytes.readUInt32LE(central + 42);
    assert.equal(bytes.readUInt32LE(central), 0x02014b50);
    assert.equal(bytes.readUInt16LE(central + 4), 0x031e);
    assert.equal(bytes.readUInt16LE(central + 6), 20);
    assert.equal(bytes.readUInt16LE(central + 8), 0x0800);
    assert.equal(bytes.readUInt16LE(central + 12), 0);
    assert.equal(bytes.readUInt16LE(central + 14), 0x21);
    assert.equal(bytes.readUInt16LE(central + 28), name.length);
    assert.deepEqual(bytes.subarray(central + 30, central + 38), Buffer.alloc(8));
    assert.equal(bytes.readUInt32LE(central + 38), (0o100644 * 65536) >>> 0);
    assert.deepEqual(bytes.subarray(central + 46, central + 46 + name.length), name);
    assert.equal(bytes.readUInt32LE(local), 0x04034b50);
    assert.deepEqual(bytes.subarray(central + 6, central + 32), bytes.subarray(local + 4, local + 30));
    methods.set(entry.name, bytes.readUInt16LE(central + 10));
    central += 46 + name.length;
  }
  assert.equal(central, end);
  assert.equal(methods.get('+asset'), 8);
  assert.equal(methods.get('@asset'), 0);
  const assets = entries.filter(entry => !entry.name.startsWith('META-INF/'));
  const expected = Buffer.from(assets.map(entry => `${hash(entry.data)}  ${entry.name}\n`).join(''));
  assert.deepEqual(await info.read('META-INF/hashes'), expected);
  const lines = (await info.read('META-INF/signature')).toString().split('\n');
  assert.equal(lines[0], 'alef-package 1'); assert.equal(lines[1], `key ${Buffer.from(key.key).toString('base64')}`); assert.equal(lines[3], '');
  const privateKey = await loadKey(undefined, { home });
  assert.ok(cryptoVerify(null, Buffer.concat([Buffer.from('alef-package-v1\n'), expected]), privateKey, Buffer.from(lines[2].slice(4), 'base64')));
  await pack(dir, { key: 'default', home }); assert.deepEqual(await readFile(result.out), bytes);
}));
test('unsigned_packing_is_explicit_and_retains_canonical_hashes_while_missing_keys_never_silently_downgrade', bounded, () => scratch(async ({ dir, home }) => {
  await assert.rejects(pack(dir, { home }), /ENOENT/);
  await assert.rejects(pack(dir, { key: 'default', unsigned: true, home }), /mutually exclusive/);
  const result = await pack(dir, { unsigned: true, home });
  assert.equal(result.status, 'Unsigned'); assert.equal(result.count, 2);
  const info = await verify(result.out); assert.equal(info.status, 'Unsigned');
  assert.deepEqual(info.entries.map(entry => entry.name), ['META-INF/hashes', 'alef.ktav']);
}));
test('normal_excludes_can_be_included_but_the_key_home_and_alef_tron_never_can', bounded, () => scratch(async ({ dir }) => {
  const home = path.join(dir, 'secret-store'); await createKey(undefined, { home });
  for (const name of ['.git', 'node_modules', '.alef-tron']) {
    await mkdir(path.join(dir, name)); await writeFile(path.join(dir, name, 'asset'), 'hidden');
  }
  const options = { home, unsigned: true };
  let info = await verify((await pack(dir, options)).out);
  assert.deepEqual(info.entries.map(entry => entry.name), ['META-INF/hashes', 'alef.ktav']);
  info = await verify((await pack(dir, { ...options, include: ['.git', 'node_modules', '.alef-tron'] })).out);
  assert.deepEqual(info.entries.map(entry => entry.name), ['.git/asset', 'META-INF/hashes', 'alef.ktav', 'node_modules/asset']);
  await assert.rejects(pack(home, options), /key home/);
}));
test('copied_private_keys_are_rejected_even_when_included', bounded, () => scratch(async ({ dir, home }) => {
  const key = await createKey(undefined, { home });
  await writeFile(path.join(dir, 'copied.pem'), await readFile(key.path));
  await assert.rejects(pack(dir, { home, unsigned: true }), /private key forbidden/);
}));
test('self_output_is_skipped_and_repeated_packing_inside_the_input_stays_deterministic', bounded, () => scratch(async ({ dir, home }) => {
  const out = path.join(dir, 'result.alef');
  await writeFile(out, 'old output');
  await pack(dir, { out, home, unsigned: true }); const first = await readFile(out);
  await pack(dir, { out, home, unsigned: true }); assert.deepEqual(await readFile(out), first);
  assert.equal((await verify(out)).entries.length, 2);
}));
test('reserved_metadata_and_missing_or_nonregular_manifests_are_rejected', bounded, () => scratch(async ({ dir, home }) => {
  const options = { home, unsigned: true };
  for (const name of ['META-INF', 'meta-inf']) {
    await mkdir(path.join(dir, name)); await assert.rejects(pack(dir, options), /reserved META-INF/); await rm(path.join(dir, name), { recursive: true });
  }
  await rm(path.join(dir, 'alef.ktav')); await assert.rejects(pack(dir, options), /alef.ktav/);
  await mkdir(path.join(dir, 'alef.ktav')); await assert.rejects(pack(dir, options), /alef.ktav/);
}));
test('unsafe_names_duplicates_symlinks_and_symlinked_output_are_refused', bounded, () => scratch(async ({ root, dir, home }) => {
  const options = { home, unsigned: true };
  if (process.platform !== 'win32') {
    for (const name of ['CON', 'bad:', 'back\\slash', 'bad.', 'bad\n', 'unicode-\u00e9', 'bad name']) {
      await writeFile(path.join(dir, name), 'x');
      await assert.rejects(pack(dir, options), error => error.message.startsWith(`${name}: ZIP:`));
      await rm(path.join(dir, name));
    }
    await writeFile(path.join(dir, 'A'), 'x'); await writeFile(path.join(dir, 'a'), 'x');
    await assert.rejects(pack(dir, options), /duplicate/); await rm(path.join(dir, 'A')); await rm(path.join(dir, 'a'));
  }
  const target = path.join(root, 'target'); await writeFile(target, 'untouched');
  try { await symlink(target, path.join(dir, 'link')); } catch (error) { if (error.code === 'EPERM') return; throw error; }
  await assert.rejects(pack(dir, options), /symlink/); await rm(path.join(dir, 'link'));
  const out = path.join(root, 'linked.alef'); await symlink(target, out);
  await assert.rejects(pack(dir, { ...options, out }), /Symlink/); assert.equal(await readFile(target, 'utf8'), 'untouched');
}));
test('directories_are_validated_but_never_emitted_as_zip_entries', bounded, () => scratch(async ({ dir, home }) => {
  const options = { home, unsigned: true };
  await mkdir(path.join(dir, 'empty'));
  await mkdir(path.join(dir, 'assets')); await writeFile(path.join(dir, 'assets', '@+file'), 'x');
  const info = await verify((await pack(dir, options)).out);
  assert.deepEqual(info.entries.map(entry => entry.name), ['META-INF/hashes', 'alef.ktav', 'assets/@+file']);
  const invalid = 'unicode-\u00e9'; await mkdir(path.join(dir, invalid));
  await assert.rejects(pack(dir, options), error => error.message.startsWith(`${invalid}: ZIP:`));
}));
test('case_insensitive_file_prefix_conflicts_are_rejected_during_metadata_preflight', bounded, () => scratch(async ({ dir, home }) => {
  if (process.platform === 'win32') return;
  await mkdir(path.join(dir, 'Asset')); await writeFile(path.join(dir, 'Asset', 'child'), 'x');
  await writeFile(path.join(dir, 'asset'), 'x');
  await assert.rejects(pack(dir, { home, unsigned: true }), /Pack: asset: file path-prefix conflict/);
}));
test('sparse_oversized_files_and_total_size_are_rejected_before_payload_allocation', bounded, () => scratch(async ({ dir, home }) => {
  const options = { home, unsigned: true };
  const file = await open(path.join(dir, 'huge'), 'w');
  try { await file.truncate(512 * 1024 * 1024 + 1); } finally { await file.close(); }
  await assert.rejects(pack(dir, options), /file size limit/); await rm(path.join(dir, 'huge'));
  for (let index = 0; index < 4; index++) {
    const handle = await open(path.join(dir, `big${index}`), 'w');
    try { await handle.truncate(512 * 1024 * 1024); } finally { await handle.close(); }
  }
  await assert.rejects(pack(dir, options), /total size limit/);
}));
test('invalid_duplicate_and_prefix_names_fail_metadata_preflight_before_any_key_or_payload_opens', bounded, () => scratch(async ({ dir, home }) => {
  const original_readdir = fs.readdir; const original_lstat = fs.lstat; const original_open = fs.open;
  const template = await original_lstat(path.join(dir, 'alef.ktav'));
  const real = await fs.realpath(dir);
  let names = []; let payload_opens = 0;
  fs.readdir = async folder => path.resolve(folder) === real ? ['alef.ktav', ...names] : original_readdir(folder);
  fs.lstat = async filename => names.some(name => path.join(real, name) === filename) ? template : original_lstat(filename);
  fs.open = async (...args) => { payload_opens++; return original_open(...args); };
  syncBuiltinESMExports();
  try {
    for (const [invalid, offending, message] of [
      [['A', 'a'], 'a', 'case-insensitive duplicate name'],
      [['a/b', 'A/B'], 'A/B', 'case-insensitive duplicate name'],
      [['a', 'A/b'], 'A/b', 'file path-prefix conflict'],
      [['a/b', 'A'], 'A', 'file path-prefix conflict'],
      [['a/b/c', 'A/B'], 'A/B', 'file path-prefix conflict'],
      [['a/b', 'A/B/c'], 'A/B/c', 'file path-prefix conflict'],
    ]) {
      names = invalid;
      for (const unsigned of [false, true]) {
        await assert.rejects(pack(dir, { home, unsigned }), error => error.message === `Pack: ${offending}: ${message}`);
        assert.equal(payload_opens, 0);
      }
    }
  } finally {
    fs.readdir = original_readdir; fs.lstat = original_lstat; fs.open = original_open;
    syncBuiltinESMExports();
  }
}));
test('entry_limits_include_generated_metadata', bounded, () => scratch(async ({ dir, home }) => {
  const original_readdir = fs.readdir; const original_lstat = fs.lstat; const original_open = fs.open;
  const template = await original_lstat(path.join(dir, 'alef.ktav'));
  const real = await fs.realpath(dir);
  let count = 9998; let file_stats = 0; let payload_opens = 0;
  fs.readdir = async folder => path.resolve(folder) === real
    ? ['alef.ktav', ...Array.from({ length: count }, (_, index) => `asset${index}`)]
    : original_readdir(folder);
  fs.lstat = async filename => {
    if (path.dirname(filename) === real && /^asset\d+$/.test(path.basename(filename))) {
      file_stats++; return template;
    }
    return original_lstat(filename);
  };
  fs.open = async (...args) => { payload_opens++; return original_open(...args); };
  syncBuiltinESMExports();
  try {
    await assert.rejects(pack(dir, { home }), /entry count limit/);
    assert.equal(file_stats, 9998); assert.equal(payload_opens, 0);
    count = 9999; file_stats = 0;
    await assert.rejects(pack(dir, { home, unsigned: true }), /entry count limit/);
    assert.equal(file_stats, 9999); assert.equal(payload_opens, 0);
  } finally {
    fs.readdir = original_readdir; fs.lstat = original_lstat; fs.open = original_open;
    syncBuiltinESMExports();
  }
}));
