// SPDX-License-Identifier: MIT OR Apache-2.0
import assert from 'node:assert/strict';
import test from 'node:test';
import { mkdtemp, writeFile, open, rename, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { deflateRawSync } from 'node:zlib';
import { readFileSync } from 'node:fs';
import { readZip, writeZip, validateName } from '../src/zip.mjs';
import { verify } from '../src/verify.mjs';
import { handmade } from './fixtures.mjs';

const MiB = 1024 * 1024;
const bounded = { timeout: 5000 };
const bytes = text => Buffer.from(text);
const item = (name = 'a', data = 'hello', method = 0) => ({ name, data: bytes(data), method });

test('directory_records_are_rejected_by_the_reader', bounded, async () => {
  await assert.rejects(readZip(handmade([item('dir/', '')])), /empty or traversal name segment/);
});

test('prefix_conflicts_are_rejected_in_both_registration_orders', bounded, async () => {
  const results = await Promise.allSettled([
    readZip(handmade([item('a'), item('a/b')])),
    readZip(handmade([item('a/b'), item('a')])),
  ]);
  assert.deepEqual(results.map(result => result.status), ['rejected', 'rejected']);
  for (const result of results) assert.match(result.reason.message, /file path-prefix conflict/);
});

test('the_signature_size_guard_precedes_payload_processing', bounded, async () => {
  await assert.rejects(readZip(handmade([{ ...item('META-INF/signature', '', 8), size: 257 }])), /signature size limit/);
});

function positions(zip) {
  const end = zip.length - 22; const central = zip.readUInt32LE(end + 16);
  return { end, central, data: 30 + zip.readUInt16LE(26) };
}
function change(zip, mutate) { const copy = Buffer.from(zip); mutate(copy, positions(copy)); return copy; }
function centralEntries(zip) {
  const result = []; let at = positions(zip).central;
  while (zip.readUInt32LE(at) === 0x02014b50) {
    result.push(at); at += 46 + zip.readUInt16LE(at + 28);
  }
  return result;
}

test('independent_stored_and_deflated_fixtures_and_writer_output_round_trip_correctly', bounded, async () => {
  const input = [item('z', '', 8), item('A', 'one'), item('nested/@+_-.txt', 'héllo', 8)];
  for (const zip of [handmade(input), writeZip(input)]) {
    const archive = await readZip(zip);
    for (const entry of input) assert.deepEqual(await archive.read(entry.name), entry.data);
    await assert.rejects(archive.read('missing'), /unknown file/);
    assert.deepEqual(await archive.entries.find(entry => entry.name === 'A').read(), bytes('one'));
  }
  assert.deepEqual((await readZip(handmade([]))).entries, []);
  assert.deepEqual((await readZip(writeZip([]))).entries, []);
  // Readers ignore attributes, never interpret them as file types/permissions.
  assert.deepEqual(await (await readZip(handmade([{ ...item(), attrs: 0xa1ff0010 }]))).read('a'), bytes('hello'));
});

test('the_writer_fixes_header_fields_sorts_names_and_selects_compression_automatically', bounded, async () => {
  const input = [{ name: 'z', data: Buffer.alloc(4096, 65) }, { name: 'a', data: bytes('x') }, { name: 'empty', data: bytes('') }];
  const zip = writeZip(input);
  assert.deepEqual(zip, writeZip(input.toReversed()));
  const archive = await readZip(zip);
  assert.deepEqual(archive.entries.map(entry => [entry.name, entry.method]), [['a', 0], ['empty', 0], ['z', 8]]);
  assert.equal(zip.readUInt16LE(positions(zip).end + 20), 0);
  let nextLocal = 0;
  for (const at of centralEntries(zip)) {
    assert.equal(zip.readUInt16LE(at + 4), 0x031e);
    assert.equal(zip.readUInt16LE(at + 6), 20);
    assert.equal(zip.readUInt16LE(at + 8), 0x0800);
    assert.equal(zip.readUInt16LE(at + 12), 0);
    assert.equal(zip.readUInt16LE(at + 14), 0x21);
    assert.equal(zip.readUInt32LE(at + 38), 0x81a40000);
    for (const field of [30, 32, 34]) assert.equal(zip.readUInt16LE(at + field), 0);
    const local = zip.readUInt32LE(at + 42); assert.equal(local, nextLocal);
    assert.equal(zip.readUInt16LE(local + 4), 20);
    assert.equal(zip.readUInt16LE(local + 6), 0x0800);
    assert.equal(zip.readUInt16LE(local + 10), 0);
    assert.equal(zip.readUInt16LE(local + 12), 0x21);
    assert.equal(zip.readUInt16LE(local + 28), 0);
    nextLocal = local + 30 + zip.readUInt16LE(local + 26) + zip.readUInt32LE(local + 18);
  }
  assert.equal(nextLocal, positions(zip).central);
  for (const method of [0, 8]) {
    assert.equal((await readZip(writeZip([{ name: 'a', data: bytes('x') }], { method }))).entries[0].method, method);
    assert.equal((await readZip(writeZip([item('a', 'x', method)]))).entries[0].method, method);
  }
  assert.throws(() => writeZip([item()], { method: 9 }), /method/);
  assert.throws(() => writeZip([item('a', '', 9)]), /method/);
  assert.throws(() => writeZip([{ name: 'a', data: 'text' }]), /bytes/);
});

test('every_forbidden_ascii_byte_unicode_name_and_malformed_encoding_is_rejected', bounded, async () => {
  for (let byte = 0; byte <= 127; byte++) {
    const char = String.fromCharCode(byte);
    if (/^[A-Za-z0-9._@+/-]$/.test(char)) continue;
    const name = `a${char}b`;
    assert.throws(() => validateName(name), /ZIP:/);
    assert.throws(() => writeZip([item(name)]), /ZIP:/);
    await assert.rejects(readZip(handmade([item(name)])), /ZIP:/);
  }
  for (const name of ['é', '中', '\ufeffa', 'a\u0085b', '\ud800']) {
    assert.throws(() => validateName(name), /ZIP:/);
    assert.throws(() => writeZip([item(name)]), /ZIP:/);
    await assert.rejects(readZip(handmade([item(name)])), /ASCII/);
  }
  for (const name of [Buffer.from([0xff]), Buffer.from([0xc0, 0xaf]), Buffer.from([0xed, 0xa0, 0x80]), Buffer.from([0xe2, 0x82])]) {
    await assert.rejects(readZip(handmade([{ ...item(), name }])), /ASCII/);
  }
});

test('invalid_segments_directories_and_device_names_are_rejected_at_exact_length_boundaries', bounded, async () => {
  const invalid = ['', '/', '/a', '.', '..', 'a/./b', 'a/../b', 'a//b', 'a/', 'a/b/', 'a.', 'a./b', 'a'.repeat(256), Array(5).fill('a'.repeat(205)).join('/')];
  for (const device of ['CON', 'PRN', 'AUX', 'NUL', ...Array.from({ length: 9 }, (_, i) => `COM${i + 1}`), ...Array.from({ length: 9 }, (_, i) => `LPT${i + 1}`)]) {
    invalid.push(device, `${device.toLowerCase()}.txt`, `x/${device}.foo.bar/y`);
  }
  const maxName = [...Array(3).fill('a'.repeat(255)), 'a'.repeat(255)].join('/') + 'b';
  // 1024 bytes with every segment <=255.
  const boundary = [...Array(4).fill('a'.repeat(204)), 'a'.repeat(204)].join('/');
  assert.equal(boundary.length, 1024);
  invalid.push(maxName, `${boundary}b`);
  for (const name of invalid) {
    assert.throws(() => validateName(name), /ZIP:/, name);
    assert.throws(() => writeZip([item(name)]), /ZIP:/);
    await assert.rejects(readZip(handmade([item(name)])), /ZIP:/, name);
  }
  for (const name of ['COM0', 'COM10', 'LPT0.txt', 'CONSOLE', 'a'.repeat(255), boundary, 'A-z_0.9@+/b']) {
    assert.equal(validateName(name), name);
    assert.equal((await readZip(handmade([item(name)]))).entries[0].name, name);
  }
});

test('ascii_case_duplicates_and_prefix_conflicts_are_rejected_in_both_orders_and_reserved_names_follow_the_allowlist', bounded, async () => {
  for (const pair of [['a', 'A'], ['a/b', 'A/B'], ['a', 'A/b'], ['a/b', 'A'], ['a/b/c', 'A/B'], ['a/b', 'A/B/c']]) {
    assert.throws(() => writeZip(pair.map(name => item(name))), /duplicate|prefix/);
    await assert.rejects(readZip(handmade(pair.map(name => item(name)))), /duplicate|prefix/);
  }
  for (const name of ['ALEF.ktav', 'meta-inf/hashes', 'META-INF/Hashes', 'META-INF/SIGNATURE', 'META-INF/other', 'meta-inf/a/b', 'META-INF/hashes/a']) {
    assert.throws(() => validateName(name), /reserved|allowlist/);
    assert.throws(() => writeZip([item(name)]), /reserved|allowlist/);
    await assert.rejects(readZip(handmade([item(name)])), /reserved|allowlist/);
  }
  const valid = ['a', 'ab/c', 'alef.ktav', 'META-INF/hashes', 'META-INF/signature', 'meta-infinite/a'];
  assert.equal((await readZip(handmade(valid.map(name => item(name))))).entries.length, valid.length);
});

test('flags_are_exact_for_both_methods_and_versions_and_methods_are_bounded', bounded, async () => {
  for (const method of [0, 8]) {
    for (const flags of [0, 1, 8, 0x802, 0x804, 0x806, ...Array.from({ length: 16 }, (_, bit) => 0x800 ^ (1 << bit))]) {
      await assert.rejects(readZip(handmade([{ ...item('a', 'data', method), flags }])), /flags/);
    }
  }
  for (const method of [1, 9, 99]) await assert.rejects(readZip(handmade([{ ...item(), method }])), /method/);
  await assert.rejects(readZip(handmade([{ ...item(), version: 45 }])), /version/);
});

test('extras_comments_descriptors_zip64_and_multidisk_archives_are_rejected', bounded, async () => {
  for (const id of [1, 0x7075, 0xbeef]) {
    const field = Buffer.alloc(4); field.writeUInt16LE(id);
    for (const key of ['localExtra', 'centralExtra']) await assert.rejects(readZip(handmade([{ ...item(), [key]: field }])), /extra/);
  }
  await assert.rejects(readZip(handmade([{ ...item(), comment: bytes('comment') }])), /comment/);
  await assert.rejects(readZip(handmade([{ ...item(), after: Buffer.alloc(16) }])), /unreferenced/);
  const descriptor = Buffer.alloc(16); descriptor.writeUInt32LE(0x08074b50);
  await assert.rejects(readZip(handmade([{ ...item(), after: descriptor }])), /unreferenced/);
  const zip = handmade([item()]);
  for (const field of [4, 6, 8]) await assert.rejects(readZip(change(zip, (copy, { end }) => { copy.writeUInt16LE(2, end + field); })), /multidisk/);
  await assert.rejects(readZip(change(zip, (copy, { central }) => { copy.writeUInt16LE(1, central + 34); })), /multidisk/);
  for (const field of [12, 16]) await assert.rejects(readZip(change(zip, (copy, { end }) => { copy.writeUInt32LE(0xffffffff, end + field); })), /ZIP64/);
  for (const field of [20, 24, 42]) await assert.rejects(readZip(change(zip, (copy, { central }) => { copy.writeUInt32LE(0xffffffff, central + field); })), /ZIP64/);
  const locator = Buffer.alloc(20); locator.writeUInt32LE(0x07064b50);
  const { end } = positions(zip);
  await assert.rejects(readZip(Buffer.concat([zip.subarray(0, end), locator, zip.subarray(end)])), /bounds/);
});

test('the_eocd_is_the_final_22_bytes_and_comments_prefixes_suffixes_concatenation_gaps_hidden_locals_and_reordered_central_entries_are_rejected', bounded, async () => {
  for (const options of [{ prefix: bytes('prefix') }, { suffix: bytes('suffix') }, { endComment: bytes('comment') }, { endComment: Buffer.from('504b0506', 'hex') }]) {
    await assert.rejects(readZip(handmade([item()], options)), /ZIP:/);
  }
  const zip = handmade([item()]);
  await assert.rejects(readZip(Buffer.concat([zip, zip])), /bounds|gap/);
  await assert.rejects(readZip(change(zip, (copy, { end }) => { copy.writeUInt16LE(1, end + 20); })), /comment/);
  await assert.rejects(readZip(change(zip, (copy, { end }) => { copy[end] = 0; })), /end record/);
  const entries = [item('b'), item('a')];
  // Reader need not sort names, but central and local orders must agree.
  assert.deepEqual((await readZip(handmade(entries))).entries.map(entry => entry.name), ['b', 'a']);
  await assert.rejects(readZip(handmade(entries, { order: [1, 0] })), /overlap|gap/);
  await assert.rejects(readZip(handmade(entries, { order: [0] })), /unreferenced/);
  await assert.rejects(readZip(handmade(entries, { order: [1] })), /gap/);
  await assert.rejects(readZip(handmade([{ ...item('a'), after: bytes('gap') }, item('b')])), /gap/);
  await assert.rejects(readZip(handmade([item('a'), { ...item('b'), offset: 0 }])), /overlap/);
  await assert.rejects(readZip(handmade([{ ...item(), compressedSize: 1000, size: 1000 }])), /bounds/);
  for (const length of [0, 4, 21, zip.length - 1]) await assert.rejects(readZip(zip.subarray(0, length)), /ZIP:/);
  await assert.rejects(readZip(change(zip, (copy, { end }) => { copy.writeUInt32LE(0, end + 12); })), /bounds/);
  await assert.rejects(readZip(change(zip, (copy, { end }) => { copy.writeUInt16LE(0, end + 8); copy.writeUInt16LE(0, end + 10); })), /size disagreement/);
});

test('local_and_central_byte_disagreements_and_crc_corruption_identify_the_entry', bounded, async () => {
  const zip = handmade([item('offending.txt', 'data')]);
  for (const field of [4, 6, 8, 10, 12, 14, 18, 22, 26]) {
    await assert.rejects(readZip(change(zip, copy => { copy[field] ^= 1; })), /offending.txt/);
  }
  await assert.rejects(readZip(handmade([{ ...item(), localName: bytes('b') }])), /local name/);
  for (const mutate of [
    copy => { copy[0] = 0; },
    (copy, { central }) => { copy[central] = 0; },
    (copy, { data }) => { copy[data] ^= 1; },
    (copy, { central }) => { copy[14] ^= 1; copy[central + 16] ^= 1; },
  ]) await assert.rejects(readZip(change(zip, mutate)), /offending.txt/);
  await assert.rejects(readZip(handmade([{ ...item(), crc: 0 }])), /CRC/);
});

test('stored_snapshot_reads_check_crc_across_chunk_boundaries_and_return_fresh_bytes', bounded, async () => {
  const data = Buffer.alloc(65536 * 2 + 17, 65);
  const archive = await readZip(handmade([{ name: 'stored', data, method: 0 }]));
  const first = await archive.read('stored');
  assert.deepEqual(first, data); first.fill(0);
  assert.deepEqual(await archive.entries[0].read(), data);
  await assert.rejects(readZip(handmade([{ name: 'stored', data, method: 0, crc: 0 }])), /stored.*CRC mismatch/);
  await assert.rejects(readZip(handmade([{ ...item('empty', ''), crc: 1 }])), /empty.*CRC mismatch/);
});

test('declared_limits_precede_payload_allocation_and_inflation', { timeout: 15000 }, async () => {
  for (const size of [512 * MiB + 1, 0xfffffffe]) {
    await assert.rejects(readZip(handmade([{ ...item('a', '', 8), size }])), /file size/);
  }
  for (const [name, size, error] of [['META-INF/hashes', 16 * MiB + 1, /hashes size/], ['META-INF/signature', 257, /signature size/]]) {
    await assert.rejects(readZip(handmade([{ ...item(name, '', 8), size }])), error);
  }
  assert.throws(() => writeZip([item('META-INF/signature', 'x'.repeat(257))]), /signature size/);
  for (const [name, size] of [['META-INF/signature', 256], ['META-INF/hashes', 16 * MiB], ['a', 512 * MiB]]) {
    await assert.rejects(readZip(handmade([{ ...item(name, '', 8), size }])), /uncompressed size disagreement/);
  }
  await assert.rejects(readZip(handmade(Array.from({ length: 4 }, (_, i) => ({ ...item(String(i), '', 8), size: 512 * MiB })))), /uncompressed size disagreement/);
  await assert.rejects(readZip(change(handmade([item()]), (copy, { end }) => { copy.writeUInt32LE(0xfffffffe, end + 16); })), /bounds/);
  await assert.rejects(readZip(handmade(Array.from({ length: 5 }, (_, i) => ({ ...item(String(i), '', 8), size: 512 * MiB })))), /total size/);
  const many = Array.from({ length: 10000 }, (_, i) => item(`f${i}`, ''));
  const valid = handmade(many); assert.equal((await readZip(valid)).entries.length, 10000);
  assert.throws(() => writeZip([...many, item('extra', '')]), /entry count/);
  await assert.rejects(readZip(change(valid, (copy, { end }) => { copy.writeUInt16LE(10001, end + 8); copy.writeUInt16LE(10001, end + 10); })), /entry count/);
  await assert.rejects(readZip(change(handmade([item()]), (copy, { end }) => { copy.writeUInt32LE(16 * MiB + 1, end + 12); })), /central directory size/);
  // Sparse files exercise actual filesystem lengths without multi-GiB allocation.
  const folder = await mkdtemp(path.join(tmpdir(), 'alef-zip-limits-'));
  try {
    const file = path.join(folder, 'sparse.zip'); const handle = await open(file, 'w');
    try {
      await handle.truncate(2304 * MiB + 1);
      await assert.rejects(readZip(file), /archive size limit/);
      const length = 16 * MiB + 23; await handle.truncate(length);
      const end = handmade([]); end.writeUInt32LE(16 * MiB + 1, 12);
      await handle.write(end, 0, 22, length - 22);
      await assert.rejects(readZip(file), /central directory size limit/);
    } finally { await handle.close(); }
  } finally { await rm(folder, { recursive: true, force: true }); }
});

test('inflation_is_bounded_by_declared_size_plus_one_and_consumes_exact_input_and_output', bounded, async () => {
  for (const size of [0, 1, 3]) await assert.rejects(readZip(handmade([{ ...item('a', 'data', 8), size }])), /exceeds declared/);
  await assert.rejects(readZip(handmade([{ ...item('a', 'data', 8), size: 5 }])), /size disagreement/);
  await assert.rejects(readZip(handmade([{ ...item('a', '', 8), size: 1, raw: deflateRawSync(Buffer.alloc(MiB)) }])), /exceeds declared/);
  for (const tail of [bytes('junk'), deflateRawSync(bytes('extra'))]) {
    await assert.rejects(readZip(handmade([{ ...item('a', 'data', 8), raw: Buffer.concat([deflateRawSync(bytes('data')), tail]) }])), /trailing compressed/);
  }
  for (const raw of [Buffer.from([0xff]), deflateRawSync(bytes('data')).subarray(0, 2)]) {
    await assert.rejects(readZip(handmade([{ ...item('a', 'data', 8), raw }])), /ZIP entry/);
  }
  await assert.rejects(readZip(handmade([{ ...item(), size: 3 }])), /stored size/);
});

test('memory_and_disk_snapshots_are_immutable_and_reads_return_fresh_results', bounded, async () => {
  const zip = handmade([item()]); const memory = await readZip(zip); zip.fill(0);
  const first = await memory.read('a'); first.fill(0);
  assert.deepEqual(await memory.read('a'), bytes('hello'));
  assert.ok(Object.isFrozen(memory)); assert.ok(Object.isFrozen(memory.entries)); assert.ok(Object.isFrozen(memory.entries[0]));
  const folder = await mkdtemp(path.join(tmpdir(), 'alef-zip-'));
  try {
    const file = path.join(folder, 'app.zip'); const replacement = path.join(folder, 'new.zip');
    await writeFile(file, handmade([item()])); const archive = await readZip(file);
    await writeFile(file, bytes('bad'));
    assert.deepEqual(await archive.read('a'), bytes('hello'));
    await writeFile(replacement, handmade([item('a', 'world')])); await rename(replacement, file);
    assert.deepEqual(await archive.read('a'), bytes('hello'));
    assert.deepEqual(await (await readZip(file)).read('a'), bytes('world'));
    await rm(file); assert.deepEqual(await archive.entries[0].read(), bytes('hello'));
    await assert.rejects(readZip(file), /ENOENT/);
    await assert.rejects(readZip(folder), /regular file/);
  } finally { await rm(folder, { recursive: true, force: true }); }
});

// Shared signing vectors (moved from vectors.test.mjs when it was merged here).
const vectorLines = readFileSync(new URL('../../../tests/fixtures/package-vectors.txt', import.meta.url), 'utf8').split('\n').filter(Boolean);

test('the_shared_vectors_are_present_and_well_formed', () => {
  assert.ok(vectorLines.length >= 80);
  const names = new Set();
  for (const line of vectorLines) {
    const [name, expect, hex] = line.split('\t');
    assert.match(expect, /^(reject|unsigned|signed:[0-9a-f]{64})$/);
    assert.match(hex, /^([0-9a-f]{2})*$/);
    assert.ok(!names.has(name)); names.add(name);
  }
});

for (const line of vectorLines) {
  const [name, expect, hex] = line.split('\t');
  test(`shared_vector_${name.replaceAll('-', '_')}`, { timeout: 20000 }, async () => {
    const bytes = Buffer.from(hex, 'hex');
    if (expect === 'reject') { await assert.rejects(verify(bytes)); return; }
    const info = await verify(bytes);
    if (expect === 'unsigned') assert.equal(info.status, 'Unsigned');
    else { assert.equal(info.status, 'Signed'); assert.equal(`signed:${info.fingerprint}`, expect); }
  });
}
