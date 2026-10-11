import test from 'node:test';
import assert from 'node:assert/strict';
import { promises as fs } from 'node:fs';
import os from 'node:os';
import path from 'node:path';
import { gzipSync, gunzipSync } from 'node:zlib';
import { main, normalize, sha256, safePath, readInventory, compareFiles, validateLocks, validateTar } from './check-patches.mjs';

function tar(entries, type = '0') {
  const chunks = [];
  for (const [name, value] of entries) {
    const bytes = Buffer.from(value), header = Buffer.alloc(512);
    header.write(name); header.write('0000644\0', 100); header.write('0000000\0', 108); header.write('0000000\0', 116);
    header.write(`${bytes.length.toString(8).padStart(11, '0')}\0`, 124); header.write('00000000000\0', 136);
    header.fill(32, 148, 156); header.write(type, 156); header.write('ustar\0', 257);
    const sum = header.reduce((a, b) => a + b, 0);
    header.write(`${sum.toString(8).padStart(6, '0')}\0 `, 148);
    chunks.push(header, bytes, Buffer.alloc((512 - bytes.length % 512) % 512));
  }
  return gzipSync(Buffer.concat([...chunks, Buffer.alloc(1024)]));
}
const manifest = '[package]\nname = "winit"\nversion = "1.2.3"\n';
async function fixture(t) {
  const root = await fs.mkdtemp(path.join(os.tmpdir(), 'alef-patches-test-'));
  t.after(() => fs.rm(root, { recursive: true, force: true }));
  const put = async (file, bytes) => { await fs.mkdir(path.dirname(path.join(root, file)), { recursive: true }); await fs.writeFile(path.join(root, file), bytes); };
  const original = new Map([['Cargo.toml', Buffer.from(manifest)], ['changed', Buffer.from('old')], ['removed', Buffer.from('gone')], ['Cargo.lock', Buffer.from('upstream')], ['same', Buffer.from([255, 13, 10, 0])]]);
  const bytes = tar([...original].map(([name, value]) => [`winit-1.2.3/${name}`, value]));
  const crate = { name: 'winit', version: '1.2.3', root: 'backend/patches/winit', sha256: sha256(bytes),
    archiveUrl: 'https://static.crates.io/crates/winit/1.2.3/download',
    checksumSources: [{ kind: 'registry-exact-version', url: 'https://index.crates.io/wi/ni/winit', version: '1.2.3', field: 'cksum' }],
    primaryLock: { path: 'backend/Cargo.lock', version: '1.2.3', source: null, checksum: null },
    files: [{ path: 'changed', status: 'changed', tracked: true, normalizedSha256: sha256(Buffer.from('new')) },
      { path: 'added', status: 'added', tracked: true, normalizedSha256: sha256(Buffer.from('add')) },
      { path: 'removed', status: 'removed', tracked: false }, { path: 'Cargo.lock', status: 'removed', tracked: false }] };
  const doc = { schemaVersion: 1, comparison: 'tracked-working-tree-vs-verified-crate', normalization: 'CRLF-to-LF-bytes-only', ignoredPublicationMetadata: ['.cargo_vcs_info.json'], crates: [crate] };
  const saveDoc = () => put('docs/PATCHES.md', `\`\`\`json\n${JSON.stringify(doc)}\n\`\`\``);
  await saveDoc();
  await put('backend/Cargo.toml', '[patch.crates-io]\nwinit = { path = "patches/winit" }\n');
  await put('backend/Cargo.lock', '[[package]]\nname = "winit"\nversion = "1.2.3"\n');
  const localManifest = `${manifest}\n[workspace]\n`;
  const local = new Map([['Cargo.toml', Buffer.from(localManifest)], ['changed', Buffer.from('new')], ['added', Buffer.from('add')], ['same', Buffer.from([255, 10, 0])]]);
  crate.files.push({ path: 'Cargo.toml', status: 'changed', tracked: true, normalizedSha256: sha256(normalize(Buffer.from(localManifest))) });
  await saveDoc();
  for (const [file, value] of local) await put(`${crate.root}/${file}`, value);
  const tracked = new Set([...local.keys()].map(file => `${crate.root}/${file}`));
  const messages = []; let requests = 0;
  const options = { root, cache: path.join(root, 'cache'), git: () => ({ tracked, winitIgnored: true }),
    stdout: msg => messages.push(msg), stderr: msg => messages.push(msg),
    fetch: async url => { requests++; return new Response(url.includes('index.crates.io') ? JSON.stringify({ name: crate.name, vers: crate.version, cksum: crate.sha256 }) : bytes); } };
  return { root, put, crate, doc, saveDoc, options, messages, tracked, bytes, original, local, requests: () => requests };
}
const settings = { timeout: 60_000 };
test('binary_normalization_preserves_invalid_utf8_bom_cr_and_final_newlines', () => {
  assert.deepEqual(normalize(Buffer.from([255, 239, 187, 191, 13, 10, 13, 0])), Buffer.from([255, 239, 187, 191, 10, 13, 0]));
  assert.notEqual(sha256(normalize(Buffer.from('x'))), sha256(normalize(Buffer.from('x\n'))));
});
test('path_and_archive_validation_rejects_unsafe_entries', () => {
  for (const name of ['../x', '/x', 'C:/x', 'a\\b', 'a/../b', 'a//b', 'a/NUL.txt', 'a.']) assert.equal(safePath(name), false);
  assert.equal(safePath('src/.hidden'), true);
  for (const name of ['../x', '/x', 'winit-1.2.3/../x', 'other/x']) assert.throws(() => validateTar(tar([[name, 'x']]), 'winit-1.2.3'), /unsafe/u);
  for (const type of ['1', '2', '3', 'x']) assert.throws(() => validateTar(tar([['winit-1.2.3/x', '']], type), 'winit-1.2.3'), /unsafe/u);
  assert.throws(() => validateTar(tar([['winit-1.2.3/x', 'a'], ['winit-1.2.3/x', 'b']]), 'winit-1.2.3'), /unsafe/u);
  assert.throws(() => validateTar(Buffer.from('bad'), 'winit-1.2.3'), /gzip/u);
});
test('gnu_long_names_are_validated_before_extraction', () => {
  const long = `winit-1.2.3/${'a'.repeat(110)}`;
  const combine = name => gzipSync(Buffer.concat([
    gunzipSync(tar([['././@LongLink', `${name}\0`]], 'L')).subarray(0, -1024),
    gunzipSync(tar([['winit-1.2.3/short', 'x']]))]));
  assert.ok(validateTar(combine(long), 'winit-1.2.3').has(long));
  assert.throws(() => validateTar(combine('winit-1.2.3/../escape'), 'winit-1.2.3'), /unsafe long name/u);
  assert.throws(() => validateTar(tar([['././@LongLink', `${long}\0`]], 'L'), 'winit-1.2.3'), /ending/u);
});
test('cold_fetch_and_verified_offline_cache_preserve_optional_winit_locks_and_exact_metadata', settings, async t => {
  const f = await fixture(t);
  assert.equal(await main(f.options), 0); assert.equal(f.requests(), 2);
  assert.equal(f.messages.at(-1), 'patches: OK (1 crates; 0 verified cache, 1 fetched)');
  f.options.fetch = async () => { throw new Error('private path'); };
  await f.put(`${f.crate.root}/Cargo.lock`, 'anything');
  await f.put(`${f.crate.root}/.cargo_vcs_info.json`, 'anything');
  assert.equal(await main(f.options), 0);
  assert.equal(f.messages.at(-1), 'patches: OK (1 crates; 1 verified cache, 0 fetched; ignored winit Cargo.lock)');
  assert.equal(f.requests(), 2);
  f.tracked.add(`${f.crate.root}/Cargo.lock`);
  assert.equal(await main(f.options), 1); assert.match(f.messages.at(-1), /Cargo.lock: status/u);
});
for (const [name, file, value, diagnostic] of [
  ['modified_listed_changed_files_are_rejected', 'changed', 'other', 'content pin'], ['modified_listed_added_files_are_rejected', 'added', 'other', 'content pin'],
  ['unlisted_new_files_are_rejected', 'new', 'x', 'unexpected untracked'], ['new_hidden_files_are_rejected', '.secret', 'x', 'unexpected untracked'],
  ['nested_publication_metadata_is_rejected', 'src/.cargo_vcs_info.json', 'x', 'unexpected untracked'],
  ['missing_listed_changed_files_are_rejected', 'changed', null, 'tracked file missing'], ['missing_listed_added_files_are_rejected', 'added', null, 'tracked file missing'],
  ['changed_files_restored_to_their_original_contents_are_rejected', 'changed', 'old', 'status unchanged'], ['restored_removed_files_are_rejected', 'removed', 'gone', 'unexpected untracked'],
  ['changes_to_unchanged_files_are_rejected', 'same', 'different', 'status changed'], ['added_original_cargo_manifests_are_rejected', 'Cargo.toml.orig', 'x', 'unexpected untracked'],
]) test(name, settings, async t => {
  const f = await fixture(t), relative = `${f.crate.root}/${file}`;
  if (value === null) await fs.rm(path.join(f.root, relative)); else await f.put(relative, value);
  assert.equal(await main(f.options), 1); assert.ok(f.messages.at(-1).includes(relative)); assert.ok(f.messages.at(-1).includes(diagnostic));
  assert.ok(!f.messages.at(-1).includes(f.root));
});
for (const file of ['changed', 'added']) {
  test(`main_rejects_listed_${file}_files_with_correct_status_and_content_but_disagreeing_git_tracking`, settings, async t => {
    const f = await fixture(t), relative = `${f.crate.root}/${file}`;
    f.tracked.delete(relative);
    assert.equal(await main(f.options), 1);
    assert.deepEqual(f.messages, [`${relative}: tracking mismatch`]);
    assert.equal(f.requests(), 2);
  });
}
test('main_accepts_listed_files_when_status_content_and_git_tracking_agree', settings, async t => {
  const f = await fixture(t);
  assert.equal(await main(f.options), 0);
  assert.deepEqual(f.messages, ['patches: OK (1 crates; 0 verified cache, 1 fetched)']);
});
test('file_statuses_independently_compare_all_three_sets', settings, async t => {
  const f = await fixture(t), tracked = new Set(f.local.keys());
  for (const file of ['changed', 'added']) {
    const local = new Map(f.local); local.delete(file); tracked.delete(file);
    assert.throws(() => compareFiles(f.crate, f.original, local, tracked), /status/u); tracked.add(file);
  }
  const local = new Map(f.local); local.set('removed', Buffer.from('gone')); tracked.add('removed');
  assert.throws(() => compareFiles(f.crate, f.original, local, tracked), /status unchanged/u);
});
test('inventory_validation_rejects_invalid_schemas_duplicates_content_pins_and_paths', settings, async t => {
  const f = await fixture(t);
  for (const mutate of [d => { d.schemaVersion = 2; }, d => { d.crates.push(d.crates[0]); },
    d => { d.crates[0].files.push(d.crates[0].files[0]); }, d => { d.crates[0].files[0].path = '../escape'; },
    d => { delete d.crates[0].files[0].normalizedSha256; }, d => { d.crates[0].files[0].ignored = true; },
    d => { d.crates[0].sha256 = 'bad'; }, d => { d.crates[0].checksumSources[0].version = '9.9.9'; }]) {
    const doc = structuredClone(f.doc); mutate(doc);
    assert.throws(() => readInventory(`\`\`\`json\n${JSON.stringify(doc)}\n\`\`\``));
  }
});
test('primary_versions_manifests_and_patch_inventories_must_match', settings, async t => {
  const f = await fixture(t);
  for (const text of ['', '[[package]]\nname = "winit"\nversion = "9.9.9"\n']) {
    await f.put('backend/Cargo.lock', text); assert.equal(await main(f.options), 1); assert.match(f.messages.at(-1), /primary/u);
  }
  await f.put('backend/Cargo.lock', '[[package]]\nname = "winit"\nversion = "1.2.3"\n');
  await f.put(`${f.crate.root}/Cargo.toml`, manifest.replace('1.2.3', '2.0.0'));
  assert.equal(await main(f.options), 1); assert.match(f.messages.at(-1), /package\/version/u);
  await f.put(`${f.crate.root}/Cargo.toml`, manifest);
  await f.put('backend/Cargo.toml', '[patch.crates-io]\n');
  assert.equal(await main(f.options), 1); assert.match(f.messages.at(-1), /patch inventory/u);
});
test('all_registry_lock_checksums_are_validated_including_nested_sources', settings, async t => {
  const f = await fixture(t), source = 'registry+https://github.com/rust-lang/crates.io-index';
  const locks = new Map([['backend/Cargo.lock', [{ name: 'winit', version: '1.2.3' }]],
    ['backend/patches/winit/nested/Cargo.lock', [{ name: 'winit', version: '1.2.3', source, checksum: 'bad' }]]]);
  assert.throws(() => validateLocks(f.crate, locks), /nested\/Cargo.lock.*checksum/u);
  locks.delete('backend/patches/winit/nested/Cargo.lock');
  f.crate.checksumSources.push({ kind: 'lock', path: 'backend/patches/winit/nested/Cargo.lock', name: 'winit', version: '1.2.3', source, field: 'checksum' });
  assert.throws(() => validateLocks(f.crate, locks), /missing checksum source/u);
  locks.set('backend/patches/winit/nested/Cargo.lock', [{ name: 'winit', version: '1.2.3', source, checksum: f.crate.sha256 }]);
  validateLocks(f.crate, locks);
});
test('network_unusable_cache_and_filesystem_errors_exit_with_code_2_without_private_paths', settings, async t => {
  const f = await fixture(t);
  f.options.fetch = async () => { throw new Error(f.root); };
  assert.equal(await main(f.options), 2); assert.equal(f.messages.at(-1), 'registry: network failure');
  await f.put('cache-file', 'x'); f.options.cache = path.join(f.root, 'cache-file');
  assert.equal(await main(f.options), 2); assert.equal(f.messages.at(-1), 'cache: unavailable');
  await fs.rm(path.join(f.root, 'backend/Cargo.lock'));
  assert.equal(await main(f.options), 1); assert.equal(f.messages.at(-1), 'backend/Cargo.lock: required file missing');
});
for (const file of ['docs/PATCHES.md', 'backend/Cargo.toml', 'backend/Cargo.lock', 'backend/patches/winit/Cargo.toml', 'backend/patches/winit/nested/Cargo.lock']) {
  test(`missing_required_file_${file.replaceAll(/[^a-z0-9]+/giu, '_').toLowerCase()}_is_reported_by_name_with_code_1`, settings, async t => {
    const f = await fixture(t);
    if (file.endsWith('nested/Cargo.lock')) { await f.put(file, ''); f.tracked.add(file); }
    if (file === `${f.crate.root}/Cargo.toml`) {
      assert.equal(f.crate.files.find(entry => entry.path === 'Cargo.toml').status, 'changed');
      assert.equal(await main(f.options), 0);
      f.messages.length = 0;
    }
    await fs.rm(path.join(f.root, file));
    assert.equal(await main(f.options), 1);
    assert.deepEqual(f.messages, [`${file}: required file missing`]);
  });
}
test('required_file_other_io_errors_remain_code_2', settings, async t => {
  const f = await fixture(t);
  await fs.rm(path.join(f.root, 'backend/Cargo.toml'));
  await fs.mkdir(path.join(f.root, 'backend/Cargo.toml'));
  assert.equal(await main(f.options), 2);
  assert.deepEqual(f.messages, ['patches: filesystem I/O failure']);
});
for (const originalCode of [0, 1, 2]) for (const fails of [false, true]) {
  test(`cleanup_${fails ? 'failure' : 'success'}_after_code_${originalCode}_${fails ? originalCode === 0 ? 'replaces_success_with_code_2_and_reports_cleanup_failure' : 'preserves_original_exit_code_and_reports_cleanup_failure' : 'preserves_original_exit_code_and_reports_success_only_for_code_0'}`, settings, async t => {
    const f = await fixture(t);
    if (originalCode === 1) await f.put(`${f.crate.root}/changed`, 'wrong');
    if (originalCode === 2) f.options.fetch = async () => { throw new Error('offline'); };
    let calls = 0;
    f.options.cleanup = async directory => {
      calls++;
      assert.equal(f.messages.some(message => message.startsWith('patches: OK')), false);
      await fs.rm(directory, { recursive: true, force: true });
      if (fails) throw new Error(directory);
    };
    assert.equal(await main(f.options), fails && originalCode === 0 ? 2 : originalCode);
    assert.equal(calls, 1);
    assert.equal(f.messages.some(message => message.startsWith('patches: OK')), originalCode === 0 && !fails);
    if (originalCode === 1) assert.equal(f.messages[0], `${f.crate.root}/changed: content pin mismatch`);
    if (originalCode === 2) assert.equal(f.messages[0], 'registry: network failure');
    assert.equal(f.messages.includes('patches: temporary cleanup failed'), fails);
    if (fails) assert.equal(f.messages.at(-1), 'patches: temporary cleanup failed');
  });
}
test('fetch_timeouts_are_bounded_even_when_injected_fetch_ignores_the_signal', { timeout: 25_000 }, async t => {
  const f = await fixture(t); f.options.fetch = () => new Promise(() => {});
  assert.equal(await main(f.options), 2); assert.equal(f.messages.at(-1), 'registry: timeout');
});
test('checksum_mismatches_are_rejected_poisoned_archives_and_records_are_repaired_and_offline_failures_are_reported', settings, async t => {
  const f = await fixture(t);
  const fetch = f.options.fetch;
  f.options.fetch = async url => url.includes('index.crates.io') ? fetch(url) : new Response('poison');
  assert.equal(await main(f.options), 1); assert.match(f.messages.at(-1), /archive checksum/u);
  f.options.fetch = async () => new Response(JSON.stringify({ name: 'winit', vers: '1.2.3', cksum: 'bad' }));
  assert.equal(await main(f.options), 1); assert.match(f.messages.at(-1), /registry checksum/u);
  f.options.fetch = fetch; assert.equal(await main(f.options), 0);
  const key = `winit-1.2.3-${f.crate.sha256}`;
  await fs.writeFile(path.join(f.options.cache, `${key}.crate`), 'poison');
  f.options.fetch = async () => { throw new Error('offline'); };
  assert.equal(await main(f.options), 2);
  f.options.fetch = fetch; assert.equal(await main(f.options), 0);
  assert.equal(f.messages.at(-1), 'patches: OK (1 crates; 0 verified cache, 1 fetched)');
  await fs.writeFile(path.join(f.options.cache, `${key}.json`), '{}');
  assert.equal(await main(f.options), 0);
  assert.equal(f.messages.at(-1), 'patches: OK (1 crates; 0 verified cache, 1 fetched)');
});
