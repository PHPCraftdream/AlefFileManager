// SPDX-License-Identifier: MIT OR Apache-2.0
import assert from 'node:assert/strict';
import test from 'node:test';
import { createHash, createPublicKey, generateKeyPairSync, sign, verify } from 'node:crypto';
import { mkdtemp, readFile, writeFile, stat, rm, symlink } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { createKey, listKeys, loadKey, keyHome } from '../src/keys.mjs';

const bounded = { timeout: 10000 };
async function scratch(run) {
  const home = await mkdtemp(path.join(tmpdir(), 'alef-keys-'));
  try { await run(home); } finally { await rm(home, { recursive: true, force: true }); }
}
test('the_keys_are_pkcs8_ed25519_with_raw32_fingerprints_and_restrictive_modes', bounded, () => scratch(async home => {
  const info = await createKey(undefined, { home });
  assert.deepEqual(Object.keys(info).sort(), ['fingerprint', 'key', 'name', 'path']);
  assert.equal(info.name, 'default'); assert.equal(info.key.length, 32);
  assert.equal(info.fingerprint, createHash('sha256').update(Buffer.from(info.key)).digest('hex'));
  const pem = await readFile(info.path, 'utf8'); assert.match(pem, /^-----BEGIN PRIVATE KEY-----\n/);
  const privateKey = await loadKey(undefined, { home });
  assert.equal(privateKey.asymmetricKeyType, 'ed25519');
  const publicKey = createPublicKey(privateKey);
  assert.deepEqual(publicKey.export({ format: 'der', type: 'spki' }).subarray(-32), Buffer.from(info.key));
  const data = Buffer.from('test'); assert.ok(verify(null, data, publicKey, sign(null, data, privateKey)));
  assert.deepEqual(await listKeys({ home }), [info]);
  if (process.platform !== 'win32') {
    assert.equal((await stat(home)).mode & 0o777, 0o700);
    assert.equal((await stat(path.join(home, 'keys'))).mode & 0o777, 0o700);
    assert.equal((await stat(info.path)).mode & 0o777, 0o600);
  }
  await assert.rejects(createKey(undefined, { home }), /EEXIST/);
  assert.equal(await readFile(info.path, 'utf8'), pem);
}));
test('exclusive_atomic_key_publication_has_exactly_one_concurrent_winner', bounded, () => scratch(async home => {
  const results = await Promise.allSettled(Array.from({ length: 8 }, () => createKey('race', { home })));
  assert.equal(results.filter(item => item.status === 'fulfilled').length, 1);
  assert.equal((await listKeys({ home })).length, 1);
}));
test('key_names_reject_traversal_windows_devices_and_unsafe_segments', bounded, () => scratch(async home => {
  for (const name of ['', '../x', 'a/b', 'a\\b', 'CON', 'nul.pem', 'COM1', 'LPT9.txt', 'a:', 'a.', 'a ', '\n', 'x'.repeat(256)]) {
    await assert.rejects(createKey(name, { home })); await assert.rejects(loadKey(name, { home }));
  }
  assert.deepEqual(await listKeys({ home }), []);
}));
test('loading_and_listing_reject_malformed_non_pkcs8_and_non_ed25519_keys', bounded, () => scratch(async home => {
  const info = await createKey('bad', { home });
  const rsa = generateKeyPairSync('rsa', { modulusLength: 1024 }).privateKey;
  for (const pem of ['junk', rsa.export({ format: 'pem', type: 'pkcs1' }), rsa.export({ format: 'pem', type: 'pkcs8' }), 'x'.repeat(16385)]) {
    await writeFile(info.path, pem);
    await assert.rejects(loadKey('bad', { home })); await assert.rejects(listKeys({ home }));
  }
}));
test('a_key_home_below_a_linked_parent_folder_works', bounded, () => scratch(async home => {
  const linked = path.join(home, 'linked');
  try { await symlink(home, linked, 'junction'); } catch (error) { if (error.code === 'EPERM') return; throw error; }
  const through = path.join(linked, 'below');
  const info = await createKey(undefined, { home: through });
  assert.equal((await listKeys({ home: through })).length, 1);
  assert.equal((await loadKey(undefined, { home: through })).asymmetricKeyType, 'ed25519');
  assert.equal(info.name, 'default');
}));
test('the_key_home_follows_the_environment_and_rejects_symlinks_without_touching_their_targets', bounded, () => scratch(async home => {
  const previous = process.env.ALEF_RUNTIME_HOME;
  try {
    process.env.ALEF_RUNTIME_HOME = home;
    assert.equal(keyHome(), home); await createKey(); assert.equal((await listKeys()).length, 1);
  } finally { if (previous === undefined) delete process.env.ALEF_RUNTIME_HOME; else process.env.ALEF_RUNTIME_HOME = previous; }
  const target = path.join(home, 'default-copy.pem');
  await writeFile(target, 'untouched');
  try { await symlink(target, path.join(home, 'keys', 'alias.pem')); } catch (error) { if (error.code === 'EPERM') return; throw error; }
  await assert.rejects(loadKey('alias', { home }), /Symlink/);
  await assert.rejects(createKey('alias', { home }), /Symlink/);
  await assert.rejects(listKeys({ home }), /Symlink/);
  assert.equal(await readFile(target, 'utf8'), 'untouched');
}));
