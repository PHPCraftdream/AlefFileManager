// SPDX-License-Identifier: MIT OR Apache-2.0
import assert from 'node:assert/strict';
import test from 'node:test';
import { createHash, createPublicKey } from 'node:crypto';
import { spawn } from 'node:child_process';
import { mkdtemp, mkdir, writeFile, readFile, rm, symlink } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { readZip, writeZip, verify, check, createKey, pack, validateName } from '../src/index.mjs';

const cliPath = fileURLToPath(new URL('../bin/alef-tron.mjs', import.meta.url));
const bounded = { timeout: 300000 };
async function scratch(run) {
  const root = await mkdtemp(path.join(tmpdir(), 'alef-cli-'));
  const dir = path.join(root, 'app'); const home = path.join(root, 'home');
  try {
    await mkdir(dir); await writeFile(path.join(dir, 'alef.ktav'), 'app: example\n');
    await run({ root, dir, home, cli: (args, cwd = dir) => invoke(args, cwd, home) });
  } finally { await rm(root, { recursive: true, force: true }); }
}
function invoke(args, cwd, home) {
  return new Promise((resolve, reject) => {
    const child = spawn(process.execPath, [cliPath, ...args], {
      cwd, env: { ...process.env, ALEF_TRON_HOME: home }, stdio: ['ignore', 'pipe', 'pipe'],
    });
    let stdout = ''; let stderr = ''; let childFailure;
    const stop = message => { childFailure ??= new Error(message); child.kill('SIGKILL'); };
    const timer = setTimeout(() => stop('CLI child exceeded its timeout'), 30000);
    child.stdout.on('data', bytes => { stdout += bytes; if (stdout.length > 65536) stop('CLI stdout exceeded its bound'); });
    child.stderr.on('data', bytes => { stderr += bytes; if (stderr.length > 65536) stop('CLI stderr exceeded its bound'); });
    child.on('error', error => { childFailure = error; });
    child.on('close', (code, signal) => {
      clearTimeout(timer);
      if (childFailure) reject(childFailure); else resolve({ code, signal, stdout, stderr });
    });
  });
}
function success(result) {
  assert.equal(result.code, 0, result.stderr); assert.equal(result.signal, null); assert.equal(result.stderr, '');
  assert.doesNotMatch(result.stdout, /PRIVATE KEY|-----BEGIN/); return result.stdout;
}
function failure(result, pattern) {
  assert.equal(result.code, 1); assert.equal(result.signal, null); assert.equal(result.stdout, '');
  assert.match(result.stderr, pattern); assert.doesNotMatch(result.stderr, /PRIVATE KEY|-----BEGIN/);
}

test('unknown_command_returns_exit_one', bounded, () => scratch(async ({ cli }) => {
  failure(await cli(['bogus']), /alef-tron: Unknown command/);
}));

test('the_public_index_exports_the_package_and_zip_operations', bounded, () => {
  for (const operation of [pack, verify, check, createKey, readZip, writeZip, validateName]) assert.equal(typeof operation, 'function');
});
test('the_keys_command_creates_lists_and_refuses_to_overwrite_keys_without_exposing_private_material', bounded, () => scratch(async ({ cli, home }) => {
  assert.equal(success(await cli(['keys', '--list'])), '');
  const first = success(await cli(['keys'])); assert.match(first, /^default [0-9a-f]{64}\nkey [A-Za-z0-9+/]{43}=\n$/);
  const pem = await readFile(path.join(home, 'keys', 'default.pem'));
  const named = success(await cli(['keys', 'release'])); assert.match(named, /^release [0-9a-f]{64}\nkey [A-Za-z0-9+/]{43}=\n$/);
  for (const output of [first, named]) {
    const [identity, publicLine] = output.trimEnd().split('\n');
    const [name, fingerprint] = identity.split(' ');
    const encoded = publicLine.slice(4); const raw = Buffer.from(encoded, 'base64');
    assert.equal(raw.length, 32); assert.equal(raw.toString('base64'), encoded);
    assert.equal(createHash('sha256').update(raw).digest('hex'), fingerprint);
    const privatePem = await readFile(path.join(home, 'keys', `${name}.pem`));
    assert.deepEqual(raw, createPublicKey(privatePem).export({ format: 'der', type: 'spki' }).subarray(-32));
  }
  const listed = success(await cli(['keys', '--list']));
  assert.equal(listed, `${first.split('\n')[0]}\n${named.split('\n')[0]}\n`);
  assert.doesNotMatch(listed, /^key /m);
  failure(await cli(['keys']), /EEXIST/);
  assert.deepEqual(await readFile(path.join(home, 'keys', 'default.pem')), pem);
  failure(await cli(['keys', '../escape']), /ZIP: empty or traversal name segment/);
  await writeFile(path.join(home, 'keys', 'release.pem'), 'not a private key');
  failure(await cli(['keys', '--list']), /PKCS8/);
}));
test('the_pack_and_verify_commands_roundtrip_default_named_and_unsigned_packages', bounded, () => scratch(async ({ cli, root, dir }) => {
  const fingerprint = success(await cli(['keys'])).split('\n')[0].split(' ')[1];
  const named = success(await cli(['keys', 'release'])).split('\n')[0].split(' ')[1];
  success(await cli(['pack', dir]));
  assert.equal(success(await cli(['verify', `${dir}.alef`])), `signed by ${fingerprint}\n`);
  const out = path.join(root, 'named.alef');
  success(await cli(['pack', dir, '--key', 'release', '--out', out]));
  assert.equal(success(await cli(['verify', out])), `signed by ${named}\n`);
  assert.equal((await readZip(out)).entries.find(entry => entry.name === 'META-INF/signature').size, 157);
  success(await cli(['pack', dir, '--unsigned', '--out', out]));
  assert.equal(success(await cli(['verify', out])), 'unsigned\n');
  failure(await cli(['pack', dir, '--key', 'missing']), /missing.pem/);
}));
test('the_include_option_supports_repetition_and_commas_but_never_includes_key_storage', bounded, () => scratch(async ({ cli, dir }) => {
  for (const name of ['.git', 'node_modules', '.alef-tron']) {
    await mkdir(path.join(dir, name)); await writeFile(path.join(dir, name, 'asset'), 'hidden');
  }
  success(await cli(['pack', dir, '--unsigned']));
  assert.equal((await verify(`${dir}.alef`)).entries.length, 2);
  for (const args of [['--include', '.git', '--include', 'node_modules', '--include', '.alef-tron'], ['--include', '.git,node_modules,.alef-tron']]) {
    success(await cli(['pack', dir, '--unsigned', ...args]));
    assert.deepEqual((await verify(`${dir}.alef`)).entries.map(entry => entry.name), ['.git/asset', 'META-INF/hashes', 'alef.ktav', 'node_modules/asset']);
  }
  const help = success(await cli(['--help'])); assert.match(help, /repeat or use commas/); assert.match(help, /validation is not performed/);
}));
test('the_cli_rejects_unknown_repeated_missing_and_conflicting_arguments_with_exit_one', bounded, () => scratch(async ({ cli, dir }) => {
  const invalid = [[], ['bogus'], ['--help', 'extra'], ['keys', '--unknown'], ['keys', '--list', '--list'], ['keys', 'name', '--list'], ['keys', 'one', 'two'], ['pack'], ['pack', dir, '--key'], ['pack', dir, '--out'], ['pack', dir, '--include'], ['pack', dir, '--key', '--unsigned'], ['pack', dir, '--unsigned', '--unsigned'], ['pack', dir, '--key', 'a', '--key', 'b'], ['pack', dir, '--out', 'a', '--out', 'b'], ['pack', dir, '--key', 'a', '--unsigned'], ['pack', dir, '--include', 'wrong'], ['pack', dir, '--include', '.git,'], ['pack', dir, '--include', '.git,.git'], ['pack', dir, '--include', '.git', '--include', '.git'], ['pack', dir, 'extra'], ['verify'], ['verify', 'a', 'b'], ['verify', '--unsigned'], ['check', 'a', 'b'], ['check', '--bad']];
  for (const args of invalid) failure(await cli(args), /alef-tron:/);
  for (const command of ['keys', 'pack', 'verify', 'check']) {
    failure(await cli([command, '']), /Empty positional argument/);
    for (const option of ['-unknown', '-']) failure(await cli([command, option]), /Unknown option:/);
  }
  for (const option of ['--key', '--out', '--include']) {
    for (const value of ['', '-release', '-', '--unsigned']) {
      failure(await cli(['pack', dir, option, value]), new RegExp(`Missing value: ${option}`));
    }
  }
}));
test('the_check_command_checks_text_hygiene_and_defaults_to_the_current_directory_without_runtime_validation', bounded, () => scratch(async ({ cli, dir, root }) => {
  const message = 'Basic check passed; full runtime manifest validation not performed.\n';
  assert.equal(success(await cli(['check'])), message); assert.equal(success(await cli(['check', dir], root)), message);
  const manifest = path.join(dir, 'alef.ktav');
  for (const [content, pattern] of [['', /nonempty/], [' \r\n', /nonempty/], [Buffer.from([0xff]), /UTF-8/], ['\ufeffapp: example\n', /BOM/], ['app:\texample\n', /tabs/], [Buffer.alloc(1024 * 1024 + 1), /size limit/]]) {
    await writeFile(manifest, content); failure(await cli(['check']), pattern);
  }
  await rm(manifest); failure(await cli(['check']), /alef.ktav.*ENOENT/);
  await mkdir(manifest); failure(await cli(['check']), /alef.ktav.*regular/); await rm(manifest, { recursive: true });
  failure(await cli(['check', path.join(root, 'missing')]), /alef.ktav/);
  await writeFile(manifest, 'This is not a runtime manifest.'); assert.equal(success(await cli(['check'])), message);
  const target = path.join(root, 'target'); await writeFile(target, 'valid text'); await rm(manifest);
  try { await symlink(target, manifest); } catch (error) { if (error.code === 'EPERM') return; throw error; }
  failure(await cli(['check']), /alef.ktav.*Symlink/); assert.equal(await readFile(target, 'utf8'), 'valid text');
}));
test('the_verify_command_names_entries_for_malformed_signatures_hash_lists_and_hash_mismatches', bounded, () => scratch(async ({ cli, dir, root }) => {
  success(await cli(['keys'])); success(await cli(['pack', dir]));
  const archive = await readZip(`${dir}.alef`); const entries = [];
  for (const entry of archive.entries) entries.push({ name: entry.name, data: await entry.read() });
  const out = path.join(root, 'bad.alef');
  for (const [name, data, pattern] of [['META-INF/signature', Buffer.from('bad signature\n'), /META-INF\/signature/], ['META-INF/hashes', Buffer.from('bad hash\n'), /META-INF\/hashes/], ['alef.ktav', Buffer.from('tampered\n'), /alef.ktav.*SHA-256 mismatch/]]) {
    await writeFile(out, writeZip(entries.map(entry => entry.name === name ? { name, data } : entry)));
    failure(await cli(['verify', out]), pattern);
  }
  const signature = entries.find(entry => entry.name === 'META-INF/signature').data.toString();
  const changed = signature.replace(/sig (.)/, (_, char) => `sig ${char === 'A' ? 'B' : 'A'}`);
  await writeFile(out, writeZip(entries.map(entry => entry.name === 'META-INF/signature' ? { ...entry, data: Buffer.from(changed) } : entry)));
  failure(await cli(['verify', out]), /META-INF\/signature/);
  await writeFile(out, 'not a zip'); failure(await cli(['verify', out]), /ZIP/);
  failure(await cli(['verify', path.join(root, 'missing.alef')]), /ENOENT/);
}));
test('the_pack_command_names_the_entry_when_private_material_or_a_manifest_is_invalid', bounded, () => scratch(async ({ cli, dir, home }) => {
  success(await cli(['keys']));
  const copy = path.join(dir, 'copied.pem'); await writeFile(copy, await readFile(path.join(home, 'keys', 'default.pem')));
  failure(await cli(['pack', dir, '--unsigned']), /copied.pem.*private key forbidden/);
  await rm(copy); await rm(path.join(dir, 'alef.ktav'));
  failure(await cli(['pack', dir, '--unsigned']), /alef.ktav/);
}));
