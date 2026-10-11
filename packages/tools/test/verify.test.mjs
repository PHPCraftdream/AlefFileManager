// SPDX-License-Identifier: MIT OR Apache-2.0
import assert from 'node:assert/strict';
import test from 'node:test';
import { createHash, generateKeyPairSync, sign } from 'node:crypto';
import { mkdtemp, writeFile, rm, rename } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { crc32 } from 'node:zlib';
import { hasSmallOrder, verify } from '../src/verify.mjs';
import { readZip } from '../src/zip.mjs';
import { handmade } from './fixtures.mjs';

const bounded = { timeout: 10000 };
const hash = bytes => createHash('sha256').update(bytes).digest('hex');
const item = (name, data) => ({ name, data: Buffer.from(data), method: 0 });
const assets = [item('alef.ktav', 'app: example'), item('index.html', '<p>Hello</p>')];
const keys = generateKeyPairSync('ed25519');
const otherKeys = generateKeyPairSync('ed25519');
const rawKey = pair => pair.publicKey.export({ format: 'der', type: 'spki' }).subarray(-32);
const hashesFor = files => Buffer.from([...files].sort((a, b) => Buffer.compare(Buffer.from(a.name), Buffer.from(b.name))).map(entry => `${hash(entry.data)}  ${entry.name}\n`).join(''));
const hashes = hashesFor(assets);
function signatureFor(list = hashes, pair = keys, embedded = rawKey(pair), prefix = 'alef-package-v1\n') {
  const signature = sign(null, Buffer.concat([Buffer.from(prefix), list]), pair.privateKey);
  return Buffer.from(`alef-package 1\nkey ${embedded.toString('base64')}\nsig ${signature.toString('base64')}\n`);
}
function packageBytes({ files = assets, list = hashes, signature, extras = [] } = {}) {
  return handmade([...files, ...(list === null ? [] : [item('META-INF/hashes', list)]), ...(signature === undefined ? [] : [item('META-INF/signature', signature)]), ...extras]);
}
async function rejectList(list, pattern = /META-INF\/hashes/) {
  for (const signature of [undefined, signatureFor(list)]) await assert.rejects(verify(packageBytes({ list, signature })), pattern);
}

test('a_corrupted_signature_is_rejected_even_with_valid_asset_hashes', bounded, async () => {
  const lines = signatureFor().toString().split('\n');
  const signature = Buffer.from(lines[2].slice(4), 'base64'); signature[40] ^= 1;
  lines[2] = `sig ${signature.toString('base64')}`;
  await assert.rejects(verify(packageBytes({ signature: Buffer.from(lines.join('\n')) })), /invalid Ed25519 signature/);
});

test('the_signature_domain_prefix_is_required', bounded, async () => {
  await assert.rejects(verify(packageBytes({ signature: signatureFor(hashes, keys, rawKey(keys), '') })), /invalid Ed25519 signature/);
});

test('unsigned_packages_without_hashes_return_frozen_interfaces_and_fresh_verified_reads', bounded, async () => {
  const info = await verify(packageBytes({ list: null }));
  assert.equal(info.status, 'Unsigned'); assert.equal(info.key, undefined);
  assert.ok(Object.isFrozen(info)); assert.ok(Object.isFrozen(info.entries));
  for (const entry of info.entries) {
    assert.ok(Object.isFrozen(entry));
    assert.deepEqual(await entry.read(), assets.find(file => file.name === entry.name).data);
  }
  const first = await info.read('alef.ktav'); first.fill(0);
  assert.deepEqual(await info.read('alef.ktav'), assets[0].data);
  await assert.rejects(info.read('missing'), /missing: unknown file/);
});

test('signed_packages_verify_exact_bytes_and_expose_immutable_key_identity_without_trust_claims', bounded, async () => {
  const info = await verify(packageBytes({ signature: signatureFor() }));
  assert.equal(info.status, 'Signed'); assert.deepEqual(Buffer.from(info.key), rawKey(keys));
  assert.equal(info.fingerprint, hash(rawKey(keys))); assert.ok(Object.isFrozen(info.key));
  assert.throws(() => { info.key[0] = 0; }, TypeError);
  assert.ok(Object.isFrozen(info)); assert.ok(Object.isFrozen(info.entries));
  for (const entry of info.entries) { assert.ok(Object.isFrozen(entry)); assert.deepEqual(await info.read(entry.name), await entry.read()); }
  assert.equal((await verify(packageBytes())).status, 'Unsigned');
  const other = await verify(packageBytes({ signature: signatureFor(hashes, otherKeys) }));
  assert.equal(other.status, 'Signed'); assert.equal(other.fingerprint, hash(rawKey(otherKeys)));
  assert.notEqual(other.fingerprint, info.fingerprint);
});

test('composition_requires_the_exact_root_manifest_and_only_exact_reserved_metadata_paths', bounded, async () => {
  for (const name of ['Alef.ktav', 'dir/alef.ktav']) await assert.rejects(verify(packageBytes({ files: [item(name, 'x')], list: null })), /alef.ktav/i);
  for (const name of ['META-INF/other', 'meta-inf/hashes', 'META-INF/Hashes', 'META-inf/signature', 'META-INF/SIGNATURE', 'meta-inf/other', 'META-INF', 'meta-inf']) {
    await assert.rejects(verify(packageBytes({ list: null, extras: [item(name, 'x')] })), error => error.message.includes(name));
  }
  await assert.rejects(verify(packageBytes({ list: null, signature: signatureFor() })), /META-INF\/signature.*missing META-INF\/hashes/);
  await assert.rejects(verify(handmade([])), /alef.ktav/);
});

test('directory_entries_and_unicode_names_are_rejected_before_package_verification', bounded, async () => {
  for (const name of ['dir/', 'META-INF/', 'META-INF/other/', 'meta-inf/mixed/', '\ue000', '😀']) await assert.rejects(verify(packageBytes({ list: null, extras: [item(name, '')] })), /ZIP/);
});

test('hashes_reject_every_bom_position_and_malformed_utf8', bounded, async () => {
  for (const at of [0, 64, 66, hashes.length - 1]) await rejectList(Buffer.concat([hashes.subarray(0, at), Buffer.from('\ufeff'), hashes.subarray(at)]), /hashes.*BOM/);
  const malformed = Buffer.from(hashes); malformed[66] = 0xff;
  await rejectList(malformed, /hashes.*UTF-8/);
});

test('hashes_require_lowercase_hex_two_spaces_and_final_lf_without_any_cr_or_empty_line', bounded, async () => {
  const value = hashes.toString();
  for (const list of [value.replace(/\n/g, '\r\n'), value.replace('alef', 'al\ref'), value.slice(0, -1), '', '\n', '\n' + value, value + '\n', value.replace('\n', '\n\n'), value.replace('  ', ' '), value.replace('  ', '\t'), value.replace('  ', '   '), value.replace(/[a-f]/, char => char.toUpperCase()), 'g' + value.slice(1), value.slice(1)]) await rejectList(Buffer.from(list));
});

test('hashes_reject_more_than_ten_thousand_lines_before_duplicate_or_set_checks', bounded, async () => {
  const line = `${hash(assets[0].data)}  alef.ktav\n`;
  await rejectList(Buffer.from(line.repeat(10001)), /hashes.*line count limit/);
  await rejectList(Buffer.from(line.repeat(10000)), /hashes.*duplicate/);
});

test('zip_rejects_oversized_hashes_and_signatures_from_declared_sizes_without_large_allocations', bounded, async () => {
  for (const [name, size, pattern] of [['META-INF/hashes', 16 * 1024 * 1024 + 1, /hashes size limit/], ['META-INF/signature', 257, /signature size limit/]]) {
    const zip = handmade([...assets, { ...item(name, ''), size, method: 8, raw: Buffer.alloc(0) }]);
    await assert.rejects(verify(zip), pattern);
  }
});

test('hashes_require_the_exact_asset_set_once_in_unsigned_ascii_byte_order', bounded, async () => {
  const lines = hashes.toString().trimEnd().split('\n');
  await rejectList(Buffer.from(lines[0] + '\n' + lines[0] + '\n' + lines[1] + '\n'), /duplicate.*alef.ktav/);
  await rejectList(Buffer.from(lines.toReversed().join('\n') + '\n'), /out-of-order.*alef.ktav/);
  await rejectList(Buffer.from(hashes.toString().replace('alef.ktav', 'Alef.ktav')), /incorrectly cased.*Alef.ktav/);
  await rejectList(Buffer.from(hashes + `${hash(Buffer.alloc(0))}  z\n`), /additional.*z/);
  await rejectList(Buffer.from(lines[0] + '\n'), /missing.*index.html/);
  await rejectList(Buffer.from(hashes + `${hash(hashes)}  META-INF/hashes\n`), /additional.*META-INF\/hashes/);
  for (const signature of [undefined, signatureFor()]) {
    await assert.rejects(verify(packageBytes({ signature, extras: [item('extra.txt', 'extra')] })), /missing.*extra.txt/);
    await assert.rejects(verify(packageBytes({ signature, files: [assets[0]] })), /additional.*index.html/);
  }
});

test('ascii_hash_order_places_uppercase_and_shorter_prefixes_before_later_bytes', bounded, async () => {
  const files = [...assets, item('Z', 'one'), item('a', 'two'), item('a+', 'three'), item('a-', 'four')];
  const list = hashesFor(files);
  assert.deepEqual(list.toString().trimEnd().split('\n').map(line => line.slice(66)), ['Z', 'a', 'a+', 'a-', 'alef.ktav', 'index.html']);
  for (const signature of [undefined, signatureFor(list)]) assert.equal((await verify(packageBytes({ files, list, signature }))).status, signature ? 'Signed' : 'Unsigned');
  const wrong = Buffer.from(list.toString().trimEnd().split('\n').toReversed().join('\n') + '\n');
  await assert.rejects(verify(packageBytes({ files, list: wrong })), /out-of-order/);
});

test('signed_and_unsigned_packages_reject_swapped_digests_and_crc_valid_payload_changes', bounded, async () => {
  const swapped = Buffer.from(assets.map((file, at) => `${hash(assets[1 - at].data)}  ${file.name}\n`).join(''));
  await rejectList(swapped, /alef.ktav.*SHA-256/);
  const changed = Buffer.from(assets[1].data); changed[0] ^= 1;
  assert.notEqual(crc32(changed), crc32(assets[1].data));
  for (const signature of [undefined, signatureFor()]) {
    const zip = packageBytes({ files: [assets[0], item('index.html', changed)], signature });
    assert.deepEqual(await (await readZip(zip)).read('index.html'), changed);
    await assert.rejects(verify(zip), /index.html.*SHA-256/);
  }
});

test('crypto_verification_rejects_corrupted_signatures_wrong_keys_prefixes_and_changed_hashes', bounded, async () => {
  const altered = Buffer.from(hashes); altered[0] = altered[0] === 48 ? 49 : 48;
  await assert.rejects(verify(packageBytes({ list: altered, signature: signatureFor() })), /signature.*invalid Ed25519 signature/);
  for (const signature of [signatureFor(hashes, keys, rawKey(otherKeys)), signatureFor(hashes, keys, rawKey(keys), '')]) await assert.rejects(verify(packageBytes({ signature })), /signature.*invalid Ed25519 signature/);
  const lines = signatureFor().toString().split('\n');
  const sig = Buffer.from(lines[2].slice(4), 'base64'); sig[40] ^= 1; lines[2] = `sig ${sig.toString('base64')}`;
  await assert.rejects(verify(packageBytes({ signature: Buffer.from(lines.join('\n')) })), /signature.*invalid Ed25519 signature/);
});

test('signature_text_requires_exactly_three_canonical_lf_lines_and_valid_utf8_without_bom', bounded, async () => {
  const valid = signatureFor().toString();
  for (const value of [valid.slice(0, -1), valid + '\n', valid.replace(/\n/g, '\r\n'), valid.replace('alef-package 1', 'alef-package 2'), valid.replace('key ', 'Key '), valid.replace('sig ', 'sig  '), valid.replace('key ', 'key\t'), valid.replace('sig ', 'SIG '), '\ufeff' + valid, valid.replace('key ', 'key \ufeff'), valid.split('\n').slice(0, 2).join('\n') + '\n']) await assert.rejects(verify(packageBytes({ signature: Buffer.from(value) })), /META-INF\/signature/);
  const malformed = signatureFor(); malformed[0] = 0xff;
  await assert.rejects(verify(packageBytes({ signature: malformed })), /signature.*UTF-8/);
});

test('signature_base64_requires_standard_padding_canonical_pad_bits_and_raw_lengths', bounded, async () => {
  const valid = signatureFor().toString();
  const alphabet = 'ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/';
  for (const label of ['key', 'sig']) {
    const original = valid.split('\n').find(line => line.startsWith(label + ' ')).slice(4);
    const at = original.indexOf('=') - 1;
    const badBits = original.slice(0, at) + alphabet[alphabet.indexOf(original[at]) + 1] + original.slice(at + 1);
    for (const replacement of [original.replace(/=+$/, ''), badBits, original + '=', original.slice(0, 4) + ' ' + original.slice(4), '_' + original.slice(1), Buffer.alloc(label === 'key' ? 31 : 63).toString('base64'), Buffer.alloc(label === 'key' ? 33 : 65).toString('base64')]) await assert.rejects(verify(packageBytes({ signature: Buffer.from(valid.replace(original, replacement)) })), /signature/);
  }
});

// The last four bytes span all CRC32 differences over GF(2), avoiding searches.
function collision(bytes) {
  const changed = Buffer.from(bytes); changed[0] ^= 1;
  const initial = crc32(changed); const basis = Array(32);
  for (let bit = 0; bit < 32; bit++) {
    const sample = Buffer.from(changed); sample[sample.length - 4 + (bit >> 3)] ^= 1 << (bit & 7);
    let value = (crc32(sample) ^ initial) >>> 0; let mask = 1n << BigInt(bit);
    for (let pivot = 31; pivot >= 0; pivot--) {
      if (((value >>> pivot) & 1) === 0) continue;
      if (basis[pivot]) { value = (value ^ basis[pivot].value) >>> 0; mask ^= basis[pivot].mask; }
      else { basis[pivot] = { value, mask }; break; }
    }
  }
  let target = (crc32(bytes) ^ initial) >>> 0; let solution = 0n;
  for (let pivot = 31; pivot >= 0; pivot--) {
    if (((target >>> pivot) & 1) === 0) continue;
    assert.ok(basis[pivot]); target = (target ^ basis[pivot].value) >>> 0; solution ^= basis[pivot].mask;
  }
  assert.equal(target, 0);
  for (let bit = 0; bit < 32; bit++) if (solution & (1n << BigInt(bit))) changed[changed.length - 4 + (bit >> 3)] ^= 1 << (bit & 7);
  assert.equal(crc32(changed), crc32(bytes)); assert.notEqual(hash(changed), hash(bytes));
  return changed;
}

test('opened_packages_keep_original_verified_snapshots_after_crc_colliding_disk_mutations', bounded, async () => {
  const folder = await mkdtemp(path.join(tmpdir(), 'alef-verify-'));
  try {
    const file = path.join(folder, 'app.alef'); const replacement = path.join(folder, 'replacement.alef');
    for (const options of [{ list: null }, {}, { signature: signatureFor() }]) {
      const zip = packageBytes(options); await writeFile(file, zip);
      const info = await verify(file); const changed = Buffer.from(zip);
      let central = zip.readUInt32LE(zip.length - 6);
      while (zip.readUInt32LE(central) === 0x02014b50) {
        const length = zip.readUInt16LE(central + 28);
        if (zip.subarray(central + 46, central + 46 + length).toString() === 'index.html') {
          const local = zip.readUInt32LE(central + 42); const data = local + 30 + length;
          collision(assets[1].data).copy(changed, data); break;
        }
        central += 46 + length;
      }
      await writeFile(file, changed);
      assert.deepEqual(await (await readZip(file)).read('index.html'), collision(assets[1].data));
      assert.deepEqual(await info.read('index.html'), assets[1].data);
      assert.deepEqual(await info.entries.find(entry => entry.name === 'index.html').read(), assets[1].data);
      if (options.list === null) assert.deepEqual(await (await verify(file)).read('index.html'), collision(assets[1].data));
      else await assert.rejects(verify(file), /index.html.*SHA-256/);
      await writeFile(replacement, changed); await rename(replacement, file);
      assert.deepEqual(await info.read('index.html'), assets[1].data);
      assert.equal(info.status, options.signature === undefined ? 'Unsigned' : 'Signed');
      assert.deepEqual(info.entries.map(entry => entry.name), [...assets.map(entry => entry.name), ...(options.list === null ? [] : ['META-INF/hashes']), ...(options.signature === undefined ? [] : ['META-INF/signature'])]);
      if (info.key) { assert.deepEqual(Buffer.from(info.key), rawKey(keys)); assert.equal(info.fingerprint, hash(rawKey(keys))); }
    }
  } finally { await rm(folder, { recursive: true, force: true }); }
});

test('memory_verification_snapshots_inputs_and_returns_fresh_payload_bytes', bounded, async () => {
  const zip = packageBytes({ signature: signatureFor() }); const input = new Uint8Array(zip);
  const info = await verify(input); input.fill(0); zip.fill(0);
  const data = await info.read('index.html'); data.fill(0);
  assert.deepEqual(await info.read('index.html'), assets[1].data);
});
test('keys_and_commitments_of_small_order_are_found_and_ordinary_points_are_not', () => {
  const identity = Buffer.alloc(32); identity[0] = 1;
  const minusOne = Buffer.alloc(32, 0xff); minusOne[0] = 0xec; minusOne[31] = 0x7f;
  const orderFour = Buffer.alloc(32);
  const wrapped = Buffer.alloc(32, 0xff); wrapped[0] = 0xee; wrapped[31] = 0x7f;
  for (const bytes of [identity, minusOne, orderFour, wrapped]) assert.equal(hasSmallOrder(bytes), true);
  const base = Buffer.alloc(32, 0x66); base[0] = 0x58;
  assert.equal(hasSmallOrder(base), false);
  const { publicKey } = generateKeyPairSync('ed25519');
  assert.equal(hasSmallOrder(publicKey.export({ format: 'der', type: 'spki' }).subarray(-32)), false);
});
