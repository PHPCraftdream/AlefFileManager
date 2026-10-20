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

const cliPath = fileURLToPath(new URL('../bin/alef-runtime.mjs', import.meta.url));
const bounded = { timeout: 300000 };
async function scratch(run) {
  const root = await mkdtemp(path.join(tmpdir(), 'alef-cli-'));
  const dir = path.join(root, 'app'); const home = path.join(root, 'home');
  try {
    await mkdir(dir); await writeFile(path.join(dir, 'alef.ktav'), 'app: example\n');
    await run({ root, dir, home, cli: (args, cwd = dir) => invoke(args, cwd, home) });
  } finally {
    // Reap any leftover owned children (never by image name): an orphaned child
    // keeps this process' event loop alive and its cwd locks make rm fail on Windows.
    const kills = [];
    for (const pid of ownedPids()) {
      kills.push(process.platform === 'win32'
        ? new Promise(resolve => {
            const killer = spawn('taskkill.exe', ['/PID', String(pid), '/T', '/F'], { stdio: 'ignore', windowsHide: true });
            killer.once('close', resolve); killer.once('error', resolve);
          })
        : Promise.resolve().then(() => { try { process.kill(-pid, 'SIGKILL'); } catch { process.kill(pid, 'SIGKILL'); } }));
    }
    await Promise.allSettled(kills);
    const reapDeadline = Date.now() + 3000;
    while (ownedPids().some(isAlive) && Date.now() < reapDeadline) await delay(50);
    // Windows lets go of the working directory of a killed process a moment after taskkill returned (EBUSY);
    // a directory that a live process still holds fails after the last retry.
    await rm(root, { recursive: true, force: true, maxRetries: 20, retryDelay: 100 });
  }
}
function invoke(args, cwd, home) {
  return new Promise((resolve, reject) => {
    const child = spawn(process.execPath, [cliPath, ...args], {
      cwd, env: { ...process.env, ALEF_RUNTIME_HOME: home }, stdio: ['ignore', 'pipe', 'pipe'],
    });
    let stdout = ''; let stderr = ''; let childFailure;
    // Kill the driver's whole tree by its PID (never by image name) so descendants holding the pipes cannot hang the test.
    const killAll = () => {
      if (!child.pid || child.exitCode !== null) return;
      if (process.platform === 'win32') spawn('taskkill.exe', ['/PID', String(child.pid), '/T', '/F'], { stdio: 'ignore', windowsHide: true });
      else { try { process.kill(-child.pid, 'SIGKILL'); } catch { try { child.kill('SIGKILL'); } catch { /* already gone */ } } }
    };
    const stop = message => { childFailure ??= new Error(message); killAll(); child.kill('SIGKILL'); };
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
  failure(await cli(['bogus']), /alef-runtime: Unknown command/);
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
  for (const name of ['.git', 'node_modules', '.alef-runtime']) {
    await mkdir(path.join(dir, name)); await writeFile(path.join(dir, name, 'asset'), 'hidden');
  }
  success(await cli(['pack', dir, '--unsigned']));
  assert.equal((await verify(`${dir}.alef`)).entries.length, 2);
  for (const args of [['--include', '.git', '--include', 'node_modules', '--include', '.alef-runtime'], ['--include', '.git,node_modules,.alef-runtime']]) {
    success(await cli(['pack', dir, '--unsigned', ...args]));
    assert.deepEqual((await verify(`${dir}.alef`)).entries.map(entry => entry.name), ['.git/asset', 'META-INF/hashes', 'alef.ktav', 'node_modules/asset']);
  }
  const help = success(await cli(['--help'])); assert.match(help, /repeat or use commas/); assert.match(help, /validation is not performed/);
}));
test('the_cli_rejects_unknown_repeated_missing_and_conflicting_arguments_with_exit_one', bounded, () => scratch(async ({ cli, dir }) => {
  const invalid = [[], ['bogus'], ['--help', 'extra'], ['keys', '--unknown'], ['keys', '--list', '--list'], ['keys', 'name', '--list'], ['keys', 'one', 'two'], ['pack'], ['pack', dir, '--key'], ['pack', dir, '--out'], ['pack', dir, '--include'], ['pack', dir, '--key', '--unsigned'], ['pack', dir, '--unsigned', '--unsigned'], ['pack', dir, '--key', 'a', '--key', 'b'], ['pack', dir, '--out', 'a', '--out', 'b'], ['pack', dir, '--key', 'a', '--unsigned'], ['pack', dir, '--include', 'wrong'], ['pack', dir, '--include', '.git,'], ['pack', dir, '--include', '.git,.git'], ['pack', dir, '--include', '.git', '--include', '.git'], ['pack', dir, 'extra'], ['verify'], ['verify', 'a', 'b'], ['verify', '--unsigned'], ['check', 'a', 'b'], ['check', '--bad']];
  for (const args of invalid) failure(await cli(args), /alef-runtime:/);
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

// Workflow coverage lives here: the test directory has its seven allowed entries.
async function fakeProject(dir, config = {}, source = "await mkdir('dist',{recursive:true}); await writeFile('dist/index.html','web');") {
  await writeFile(path.join(dir, 'fake-build.mjs'), `import {mkdir,writeFile} from 'node:fs/promises';\n${source}\n`);
  const command = `"${process.execPath}" fake-build.mjs`;
  await writeFile(path.join(dir, 'package.json'), JSON.stringify({ 'alef-runtime': { build: command, ...config } }));
  return command;
}
async function workflow() {
  return { ...await import('../src/workflow/project.mjs'), ...await import('../src/index.mjs') };
}

test('build assembles unchanged manifest root icon and web tree with pack roundtrip', bounded, () => scratch(async ({ dir, root }) => {
  const { build } = await workflow();
  const bytes = Buffer.from('id: org.example.app\r\nversion:: 1.0.0\r\nassets: web/build\r\n');
  await writeFile(path.join(dir, 'alef.ktav'), bytes);
  await writeFile(path.join(dir, 'icon.png'), 'icon');
  await fakeProject(dir);
  const result = await build(dir, { stdio: 'ignore' });
  assert.equal(result.dir, path.join(await realpath(dir), 'app-build'));
  assert.deepEqual(await readFile(path.join(result.dir, 'alef.ktav')), bytes);
  assert.equal(await readFile(path.join(result.dir, 'icon.png'), 'utf8'), 'icon');
  assert.equal(await readFile(path.join(result.dir, 'web/build/index.html'), 'utf8'), 'web');
  const out = path.join(root, 'built.alef');
  await pack(result.dir, { unsigned: true, out });
  assert.deepEqual((await verify(out)).entries.map(entry => entry.name), ['META-INF/hashes', 'alef.ktav', 'icon.png', 'web/build/index.html']);
}));

test('shared reader validates supported assets spellings and rejects unsafe or uncertain encodings', bounded, () => scratch(async ({ dir }) => {
  const { readProject } = await workflow();
  for (const line of ['assets: web/build', '"assets": "web/build"', "'assets':: web/build", '`assets`: `web/build`', 'assets: ((\nweb/build\n))']) {
    await writeFile(path.join(dir, 'alef.ktav'), `## assets: decoy\n${line}\n`);
    assert.equal((await readProject(dir)).manifest.assets, 'web/build'); await check(dir);
  }
  for (const value of ['../dist', './dist', '/dist', 'a//b', 'a\\b', 'NUL.txt', 'COM1/a', 'dist.', 'a~1', 'meta-inf/a', 'alef.ktav/a', 'icon.png/a', 'null', 'true', '123', '[]', '{}', '"a\\u002fb"', '((\ndist \n))']) {
    await writeFile(path.join(dir, 'alef.ktav'), `assets: ${value}\n`);
    await assert.rejects(check(dir), /assets/); await assert.rejects(readProject(dir), /assets/);
  }
  for (const text of ['assets: dist\nassets: other\n', '"a\\u0073sets": dist\n', '{ assets: dist, name: x }']) {
    await writeFile(path.join(dir, 'alef.ktav'), text); await assert.rejects(check(dir), /assets/);
  }
  await writeFile(path.join(dir, 'alef.ktav'), 'windows: [{ assets: nested }]\napp: example\n');
  assert.equal((await readProject(dir)).manifest.assets, 'dist');
}));

test('build rejects dangerous destinations before commands or deletion', bounded, () => scratch(async ({ root, dir }) => {
  const { build } = await workflow();
  await fakeProject(dir, {}, "await writeFile('ran','yes'); await mkdir('dist',{recursive:true});");
  await mkdir(path.join(dir, 'protected')); await writeFile(path.join(dir, 'protected/package.json'), '{}');
  await mkdir(path.join(dir, 'repo/.git'), { recursive: true });
  for (const out of [dir, root, path.join(dir, '..', 'app'), 'dist', 'dist/nested', 'protected', 'repo', path.join(dir, 'alef.ktav')]) {
    await assert.rejects(build(dir, { out, stdio: 'ignore' }), /Unsafe|Protected/);
    await assert.rejects(readFile(path.join(dir, 'ran')), /ENOENT/);
  }
  if (process.platform === 'win32') {
    const { spawnSync } = await import('node:child_process');
    const short = spawnSync(process.env.ComSpec || 'cmd.exe', ['/d', '/c', `for %I in ("${dir}") do @echo %~sI`], { encoding: 'utf8', timeout: 5000 }).stdout?.trim();
    if (short) await assert.rejects(build(dir, { out: short, stdio: 'ignore' }), /Unsafe|Protected/);
  }
  const alias = path.join(root, 'alias');
  try { await symlink(dir, alias, process.platform === 'win32' ? 'junction' : 'dir'); }
  catch (error) { if (error.code === 'EPERM') return; throw error; }
  await assert.rejects(build(dir, { out: alias, stdio: 'ignore' }), /Symlink|Unsafe/);
  await assert.rejects(build(dir, { out: path.join(alias, 'dist', 'absent'), stdio: 'ignore' }), /overlaps source/);
  await mkdir(path.join(dir, 'nested-output'));
  await symlink(path.join(root, 'alias'), path.join(dir, 'nested-output/link'), process.platform === 'win32' ? 'junction' : 'dir');
  await assert.rejects(build(dir, { out: 'nested-output', stdio: 'ignore' }), /Symlink.*link/);
  // Parent aliases are allowed when the canonical destination is safe.
  const result = await build(alias, { out: path.join(alias, 'assembled'), stdio: 'ignore' });
  assert.equal(result.dir, path.join(await realpath(dir), 'assembled'));
}));

test('build rejects missing outDir symlink entries and command failures with bounded timeouts', bounded, () => scratch(async ({ root, dir }) => {
  const { build } = await workflow();
  await fakeProject(dir, {}, '');
  await assert.rejects(build(dir, { stdio: 'ignore' }), /Missing outDir/);
  await fakeProject(dir, {}, 'process.exit(7);');
  await assert.rejects(build(dir, { stdio: 'ignore' }), /exit 7/);
  await fakeProject(dir, {}, 'setInterval(() => {}, 1000);');
  await assert.rejects(build(dir, { timeoutMs: 200, stdio: 'ignore' }), /timed out/);
  await fakeProject(dir);
  await mkdir(path.join(dir, 'dist')); await writeFile(path.join(root, 'outside'), 'untouched');
  try { await symlink(path.join(root, 'outside'), path.join(dir, 'dist/linked')); }
  catch (error) { if (error.code === 'EPERM') return; throw error; }
  await assert.rejects(build(dir, { stdio: 'ignore' }), /Symlink.*linked/);
  assert.equal(await readFile(path.join(root, 'outside'), 'utf8'), 'untouched');
}));

test('project config hygiene and timeout validation precede command side effects', bounded, () => scratch(async ({ dir }) => {
  const { build, readProject } = await workflow();
  await fakeProject(dir, {}, "await writeFile('ran','yes');");
  await writeFile(path.join(dir, 'alef.ktav'), 'app:\tx\n');
  await assert.rejects(build(dir, { stdio: 'ignore' }), /tabs/);
  await assert.rejects(readFile(path.join(dir, 'ran')), /ENOENT/);
  await writeFile(path.join(dir, 'alef.ktav'), 'app: example\n');
  for (const config of [null, [], { build: 1 }, { build: '' }, { dev: [] }, { outDir: '' }, { devUrl: 'file:///tmp' }]) {
    await writeFile(path.join(dir, 'package.json'), JSON.stringify({ 'alef-runtime': config }));
    await assert.rejects(readProject(dir), /package.json/);
  }
  await writeFile(path.join(dir, 'package.json'), '{'); await assert.rejects(readProject(dir), /package.json/);
  await fakeProject(dir);
  for (const timeoutMs of [0, -1, Infinity, 1.5, '100']) await assert.rejects(build(dir, { timeoutMs }), /timeoutMs/);
  await assert.rejects(readFile(path.join(dir, 'ran')), /ENOENT/);
}));

test('project npm defaults and optional package config execute fixed script names', bounded, () => scratch(async ({ dir }) => {
  const { build, readProject } = await workflow();
  const empty = await readProject(dir); assert.equal(empty.devUrl, undefined); assert.equal(empty.outDir, path.join(await realpath(dir), 'dist'));
  await assert.rejects(build(dir), /no build command/);
  await fakeProject(dir);
  const command = 'node fake-build.mjs';
  await writeFile(path.join(dir, 'package.json'), JSON.stringify({ scripts: { build: command, dev: command } }));
  const project = await readProject(dir);
  assert.deepEqual(project.build, { npm: 'build' }); assert.deepEqual(project.dev, { npm: 'dev' });
  const result = await build(dir, { stdio: 'ignore', timeoutMs: 30000 });
  assert.equal(await readFile(path.join(result.dir, 'dist/index.html'), 'utf8'), 'web');
}));

// Dev workflow coverage: real loopback servers, fake runtime shims, bounded waits.
import { realpath, stat } from 'node:fs/promises';
import net from 'node:net';
import { pathToFileURL } from 'node:url';
import { dev, devtoolsArg, findRuntime, ownedPids } from '../src/workflow/dev.mjs';

const isAlive = pid => { try { process.kill(pid, 0); return true; } catch { return false; } };
const delay = ms => new Promise(resolve => setTimeout(resolve, ms));
async function assertDead(pid) {
  const deadline = Date.now() + 5000;
  while (isAlive(pid) && Date.now() < deadline) await new Promise(resolve => setTimeout(resolve, 50));
  assert.ok(!isAlive(pid), `pid ${pid} should have exited`);
}
async function freePort() {
  return await new Promise((resolve, reject) => {
    const server = net.createServer();
    server.once('error', reject);
    server.listen(0, '127.0.0.1', () => { const { port } = server.address(); server.close(() => resolve(port)); });
  });
}
async function devProject(dir, port, { devUrlPort, dieAfterMs } = {}) {
  const exit = dieAfterMs === undefined ? '' : `setTimeout(() => process.exit(3), ${dieAfterMs});\n`;
  await writeFile(path.join(dir, 'fake-dev.mjs'),
    `import http from 'node:http';import{appendFile,writeFile} from 'node:fs/promises';\n` +
    `await writeFile(${JSON.stringify(path.join(dir, 'dev-pid.txt'))},String(process.pid));\n` +
    `http.createServer((q,s)=>{appendFile(${JSON.stringify(path.join(dir, 'dev-log.txt'))},'hit\\n').catch(()=>{});s.end('ok');})` +
    `.listen(${port},'127.0.0.1');\n${exit}`);
  await writeFile(path.join(dir, 'package.json'), JSON.stringify({ 'alef-runtime': {
    dev: `"${process.execPath}" fake-dev.mjs`, devUrl: `http://127.0.0.1:${devUrlPort ?? port}`,
  } }));
}
const devPid = async dir => Number(await readFile(path.join(dir, 'dev-pid.txt'), 'utf8'));
async function runtimeShim(root) {
  const log = path.join(root, 'runtime-log.json');
  const pid = path.join(root, 'runtime-pid.txt');
  const script = path.join(root, 'fake-runtime.mjs');
  await writeFile(script,
    `import{writeFile} from 'node:fs/promises';import{spawn} from 'node:child_process';
` +
    `const grandchild = ${JSON.stringify(path.join(root, 'runtime-grandchild.txt'))};
` +
    `let extra = null;
` +
    `if (process.env.FAKE_RUNTIME_GRANDCHILD) {
` +
    `  extra = spawn(process.execPath, ['-e','setTimeout(()=>{},60000)'], { stdio: 'ignore' });
` +
    `  await writeFile(grandchild, String(extra.pid));
` +
    `}
` +
    `await Promise.all([writeFile(${JSON.stringify(log)},JSON.stringify(process.argv.slice(2))),` +
    `writeFile(${JSON.stringify(pid)},String(process.pid))]);
` +
    `if (process.env.FAKE_RUNTIME_ASSERT_LAST) {
` +
    `  const args = process.argv.slice(2);
` +
    `  const separator = args.indexOf('--');
` +
    `  const last = args.indexOf(process.env.FAKE_RUNTIME_ASSERT_LAST);
` +
    `  if (last !== -1 && (separator === -1 || separator > last)) process.exit(9);
` +
    `}
` +
    `await new Promise(resolve => setTimeout(resolve, Number(process.env.FAKE_RUNTIME_LIVE_MS ?? 0)));
` +
    `process.exit(Number(process.env.FAKE_RUNTIME_EXIT ?? 0));
`);
  const shim = path.join(root, process.platform === 'win32' ? 'runtime-shim.cmd' : 'runtime-shim');
  const command = `"${process.execPath}" "${script}"`;
  await writeFile(shim, process.platform === 'win32' ? `@echo off\r\n${command} %*\r\n` : `#!/bin/sh\nexec ${command} "$@"\n`, { mode: 0o755 });
  return { shim, log, pid };
}

test('dev starts the server waits for the url then runs the runtime with exact argv and passthrough args', bounded, () => scratch(async ({ dir, root }) => {
  const port = await freePort();
  await devProject(dir, port);
  const { shim, log } = await runtimeShim(root);
  process.env.FAKE_RUNTIME_ASSERT_LAST = '--help';
  try {
    assert.deepEqual(await dev(dir, { alefBin: shim, devStdio: 'ignore', appArgs: ['--help'] }), { code: 0 });
  } finally { delete process.env.FAKE_RUNTIME_ASSERT_LAST; }
  assert.deepEqual(JSON.parse(await readFile(log, 'utf8')),
    ['--app', await realpath(dir), '--dev-url', `http://127.0.0.1:${port}`, '--', '--help']);
  assert.match(await readFile(path.join(dir, 'dev-log.txt'), 'utf8'), /hit/);
  // Ordering: the dev server answered (and logged) before the runtime was spawned.
  assert.ok((await stat(path.join(dir, 'dev-log.txt'))).mtimeMs <= (await stat(log)).mtimeMs + 50);
  await assertDead(await devPid(dir));
  assert.deepEqual(ownedPids(), []);
  // No app arguments: no trailing separator, exact argv.
  assert.deepEqual(await dev(dir, { alefBin: shim, devStdio: 'ignore' }), { code: 0 });
  assert.deepEqual(JSON.parse(await readFile(log, 'utf8')),
    ['--app', await realpath(dir), '--dev-url', `http://127.0.0.1:${port}`]);
  await assertDead(await devPid(dir));
  assert.deepEqual(ownedPids(), []);
  // devtools: true becomes a bare --devtools before any -- separator.
  assert.deepEqual(await dev(dir, { alefBin: shim, devStdio: 'ignore', devtools: true }), { code: 0 });
  assert.deepEqual(JSON.parse(await readFile(log, 'utf8')),
    ['--app', await realpath(dir), '--dev-url', `http://127.0.0.1:${port}`, '--devtools']);
  await assertDead(await devPid(dir));
  assert.deepEqual(ownedPids(), []);
}));

test('dev rejects unsafe cmd.exe metacharacters in app arguments before spawning the .cmd shim', bounded, () => scratch(async ({ dir, root }) => {
  const port = await freePort();
  await devProject(dir, port);
  const { shim } = await runtimeShim(root);
  if (process.platform !== 'win32') return; // the shim is a plain executable on posix
  await assert.rejects(dev(dir, { alefBin: shim, devStdio: 'ignore', appArgs: ['a&b'] }),
    /cmd\.exe metacharacter/);
  assert.deepEqual(ownedPids(), []);
  await assert.rejects(readFile(path.join(root, 'runtime-pid.txt')), /ENOENT/);
}));

test('dev resolves the runtime exit code and kills the dev server when the runtime exits', bounded, () => scratch(async ({ dir, root }) => {
  const port = await freePort();
  await devProject(dir, port);
  const { shim } = await runtimeShim(root);
  process.env.FAKE_RUNTIME_EXIT = '7';
  try { assert.deepEqual(await dev(dir, { alefBin: shim, devStdio: 'ignore' }), { code: 7 }); }
  finally { delete process.env.FAKE_RUNTIME_EXIT; }
  await assertDead(await devPid(dir));
  assert.deepEqual(ownedPids(), []);
}));

test('dev kills the runtime and reports the reason when the dev server dies first', bounded, () => scratch(async ({ dir, root }) => {
  const port = await freePort();
  await devProject(dir, port, { dieAfterMs: 1500 });
  const { shim } = await runtimeShim(root);
  process.env.FAKE_RUNTIME_LIVE_MS = '5000';
  process.env.FAKE_RUNTIME_GRANDCHILD = '1';
  let grandchildPid;
  try {
    await assert.rejects(dev(dir, { alefBin: shim, devStdio: 'ignore' }), /Dev server died.*exit 3/);
    grandchildPid = Number(await readFile(path.join(root, 'runtime-grandchild.txt'), 'utf8'));
  } finally { delete process.env.FAKE_RUNTIME_LIVE_MS; delete process.env.FAKE_RUNTIME_GRANDCHILD; }
  await assertDead(await devPid(dir));
  await assertDead(Number(await readFile(path.join(root, 'runtime-pid.txt'), 'utf8')));
  if (grandchildPid) await assertDead(grandchildPid);
  assert.deepEqual(ownedPids(), []);
}));

test('dev times out kills the dev server and rejects when the url never answers', bounded, () => scratch(async ({ dir, root }) => {
  const port = await freePort();
  await devProject(dir, port, { devUrlPort: 1 }); // nothing listens on port 1
  const { shim } = await runtimeShim(root);
  await assert.rejects(dev(dir, { alefBin: shim, devStdio: 'ignore', readyTimeoutMs: 300 }),
    /not ready within 300 ms/);
  await assertDead(await devPid(dir));
  assert.deepEqual(ownedPids(), []);
}));

test('dev rejects a missing dev command or missing devUrl and invalid timeouts before spawning anything', bounded, () => scratch(async ({ dir }) => {
  await writeFile(path.join(dir, 'package.json'), JSON.stringify({ 'alef-runtime': { devUrl: 'http://127.0.0.1:9' } }));
  await assert.rejects(dev(dir, { devStdio: 'ignore' }), /no dev command or scripts\.dev/);
  await writeFile(path.join(dir, 'package.json'), JSON.stringify({ 'alef-runtime': { dev: 'node nothing.mjs' } }));
  await assert.rejects(dev(dir, { devStdio: 'ignore' }), /dev command without alef-runtime\.devUrl/);
  await devProject(dir, 1);
  for (const readyTimeoutMs of [0, -1, 1.5, '100', Infinity]) await assert.rejects(dev(dir, { readyTimeoutMs, devStdio: 'ignore' }), /timeoutMs/);
  assert.deepEqual(ownedPids(), []);
}));

test('findRuntime resolves ALEF_BIN runtime module PATH and reports all three places when absent', bounded, () => scratch(async ({ root, dir }) => {
  const saved = process.env.ALEF_BIN; delete process.env.ALEF_BIN;
  try {
    assert.equal(await findRuntime(dir, { alefBin: 'custom-alef' }), 'custom-alef');
    const modulePath = path.join(root, 'fake-runtime-module.mjs');
    await writeFile(modulePath, 'export function getRuntimePath() { return "module-alef"; }\n');
    assert.equal(await findRuntime(dir, { runtimeModule: pathToFileURL(modulePath).href }), 'module-alef');
    const bin = path.join(root, 'bin'); await mkdir(bin);
    const name = process.platform === 'win32' ? 'alef.exe' : 'alef';
    await writeFile(path.join(bin, name), 'x', { mode: 0o755 });
    assert.equal(await findRuntime(dir, { path: bin }), path.join(bin, name));
    await mkdir(path.join(root, 'empty'));
    await assert.rejects(findRuntime(dir, { path: path.join(root, 'empty') }), /ALEF_BIN.*getRuntimePath\(\).*PATH/);
  } finally { if (saved !== undefined) process.env.ALEF_BIN = saved; }
}));

test('dev kills both owned children and exits nonzero when interrupted on posix', { skip: process.platform === 'win32', timeout: 300000 }, () => scratch(async ({ dir, root }) => {
  // On win32 child.kill('SIGINT') is unreliable, so this test is posix-only.
  const port = await freePort();
  await devProject(dir, port);
  const { shim, pid: runtimePid } = await runtimeShim(root);
  const driver = path.join(root, 'signal-driver.mjs');
  await writeFile(driver, `import { dev } from ${JSON.stringify(pathToFileURL(fileURLToPath(new URL('../src/workflow/dev.mjs', import.meta.url))).href)};\n` +
    'try { await dev(process.argv[2], { alefBin: process.argv[3], devStdio: "ignore" }); process.exit(process.exitCode ?? 0); }\n' +
    'catch (error) { console.error(error.message); process.exit(process.exitCode ?? 1); }\n');
  // Detached on posix so the tree-kill safety net can signal the driver's whole group.
  // The runtime must outlive the signal: it lives a minute and starts a grandchild of its own.
  const env = { ...process.env, FAKE_RUNTIME_LIVE_MS: '60000', FAKE_RUNTIME_GRANDCHILD: '1' };
  const child = spawn(process.execPath, [driver, dir, shim], { env, stdio: ['ignore', 'pipe', 'pipe'], detached: process.platform !== 'win32' });
  const deadline = Date.now() + 10000;
  while (!(await exists(path.join(root, 'runtime-log.json'))) && Date.now() < deadline) await delay(50);
  assert.ok(await exists(path.join(root, 'runtime-log.json')), 'runtime did not start within 10 s');
  child.kill('SIGINT');
  // Bound the driver close; on expiry kill the DRIVER's whole tree by its PID and fail instead of hanging.
  const [code] = await new Promise(resolve => {
    const killer = setTimeout(() => {
      if (process.platform === 'win32') spawn('taskkill.exe', ['/PID', String(child.pid), '/T', '/F'], { stdio: 'ignore', windowsHide: true });
      else { try { process.kill(-child.pid, 'SIGKILL'); } catch { try { child.kill('SIGKILL'); } catch { /* gone */ } } }
      resolve([null, null]);
    }, 15000);
    child.once('close', (c, s) => { clearTimeout(killer); resolve([c, s]); });
  });
  assert.equal(code, 130);
  await assertDead(await devPid(dir)); await assertDead(Number(await readFile(runtimePid, 'utf8')));
  await assertDead(Number(await readFile(path.join(root, 'runtime-grandchild.txt'), 'utf8')));
  assert.deepEqual(ownedPids(), []);
}));

test('the build command prints the directory and the dev command validates its usage', bounded, () => scratch(async ({ cli, dir }) => {
  await fakeProject(dir);
  assert.equal(success(await cli(['build', dir])).trim(), path.join(await realpath(dir), 'app-build'));
  assert.match(success(await cli(['--help'])), /alef-runtime build/);
  assert.match(success(await cli(['--help'])), /alef-runtime dev/);
  failure(await cli(['build', dir, 'extra']), /Usage: build/);
  failure(await cli(['dev', dir, 'extra']), /Usage: dev/);
  failure(await cli(['dev', dir, '--ready-timeout', 'x']), /--ready-timeout must be a positive integer/);
  failure(await cli(['dev', dir, '--ready-timeout']), /Missing value: --ready-timeout/);
  // Devtools validation happens in the CLI before dev() reads any project.
  assert.match(success(await cli(['--help'])), /--devtools\[=PORT\]/);
  const missing = path.join(await realpath(dir), '..', 'no-such-project');
  failure(await cli(['dev', missing, '--devtools=0']), /--devtools must be an integer between 1 and 65535/);
  failure(await cli(['dev', missing, '--devtools=abc']), /--devtools must be an integer between 1 and 65535/);
  failure(await cli(['dev', dir, '--devtools', '--devtools']), /--devtools is given twice/);
}));

test('devtoolsArg maps the option to exact runtime argv pieces', () => {
  assert.deepEqual(devtoolsArg(true), ['--devtools']);
  assert.deepEqual(devtoolsArg(7000), ['--devtools=7000']);
  assert.deepEqual(devtoolsArg(undefined), []);
});

test('the dev command starts the server reports the missing runtime and kills the server', bounded, () => scratch(async ({ cli, dir }) => {
  const port = await freePort();
  await devProject(dir, port);
  const saved = process.env.ALEF_BIN; delete process.env.ALEF_BIN;
  try { failure(await cli(['dev', dir, '--ready-timeout', '15000']), /runtime executable not found/i); }
  finally { if (saved !== undefined) process.env.ALEF_BIN = saved; }
  await assertDead(await devPid(dir));
}));

test('build rejects an ancestor output before the command runs and still builds to a safe sibling with outDir outside the project', bounded, () => scratch(async ({ root, dir }) => {
  const { build } = await workflow();
  const sibling = path.join(root, 'out');
  await writeFile(path.join(dir, 'alef.ktav'), 'app: example\nassets: web/build\n');
  await mkdir(path.join(sibling, 'dist'), { recursive: true });
  await writeFile(path.join(sibling, 'dist', 'index.html'), 'web');
  await fakeProject(dir, { outDir: sibling }, "await writeFile('ran','yes');");
  await assert.rejects(build(dir, { out: root, stdio: 'ignore' }), /Protected output entry/);
  await assert.rejects(readFile(path.join(dir, 'ran')), /ENOENT/);
  const result = await build(dir, { out: path.join(root, 'safe'), stdio: 'ignore' });
  assert.equal(await readFile(path.join(dir, 'ran'), 'utf8'), 'yes');
  assert.equal(await readFile(path.join(result.dir, 'web/build/dist/index.html'), 'utf8'), 'web');
}));

test('the_cli_dev_command_propagates_the_runtime_exit_code_and_leaves_no_server_behind', bounded, () => scratch(async ({ cli, dir, root }) => {
  const port = await freePort();
  await devProject(dir, port);
  const { shim } = await runtimeShim(root);
  const saved = process.env.ALEF_BIN;
  process.env.ALEF_BIN = shim;
  try {
    process.env.FAKE_RUNTIME_EXIT = '7';
    try { const result = await cli(['dev', dir, '--ready-timeout', '15000']); assert.equal(result.code, 7); assert.equal(result.stderr, ''); }
    finally { delete process.env.FAKE_RUNTIME_EXIT; }
    const clean = await cli(['dev', dir, '--ready-timeout', '15000']);
    assert.equal(clean.code, 0); assert.equal(clean.stderr, '');
  } finally { if (saved !== undefined) process.env.ALEF_BIN = saved; else delete process.env.ALEF_BIN; }
  assert.match(await readFile(path.join(dir, 'dev-log.txt'), 'utf8'), /hit/);
  await assertDead(await devPid(dir));
}));

async function exists(file) { try { await stat(file); return true; } catch { return false; } }

test('killTree returns only after its process is reaped', { skip: process.platform === 'win32', timeout: 60000 }, () => scratch(async ({ dir }) => {
  const { killTree, startCommand } = await import('../src/workflow/process.mjs');
  // The shell execs a program that ignores SIGTERM, so only SIGKILL ends it.
  const shell = `exec "${process.execPath}" -e "process.on('SIGTERM', () => {}); setTimeout(() => {}, 60000)"`;
  const child = startCommand({ shell }, { cwd: dir, stdio: 'ignore' });
  await delay(500);
  await killTree(child);
  // No polling here: a process that is still dying or not yet reaped when killTree returns fails.
  assert.throws(() => process.kill(child.pid, 0), { code: 'ESRCH' });
}));

test('the bundle command validates usage and formats and produces a dir bundle', bounded, () => scratch(async ({ cli, root }) => {
  const usage = /Usage: bundle <PACKAGE\.alef>/;
  failure(await cli(['bundle']), usage);
  failure(await cli(['bundle', 'p.alef']), usage); // missing --platform
  failure(await cli(['bundle', 'p.alef', '--platform', 'win32-x64', '--bogus']), /Unknown option: --bogus/);
  failure(await cli(['bundle', 'p.alef', '--format', 'deb', '--platform', 'win32-x64']),
    /Bundle: format deb is not available for platform win32-x64/); // format/platform validation precedes file reads
  // Repeated --format is accepted; the failure comes later from the missing runtime.
  failure(await cli(['bundle', 'p.alef', '--format', 'tar.gz', '--format', 'dir', '--platform', 'linux-x64']),
    /Bundle: runtime binary path is required/);
  // Successful dir-format run: key, signed package, fake runtime binary.
  success(await cli(['keys']));
  const app = path.join(root, 'bapp'); await mkdir(path.join(app, 'dist'), { recursive: true });
  await writeFile(path.join(app, 'alef.ktav'),
    'id: com.example.app\nname:: Sample App\nversion:: 1.2.3\nruntime:: ^1.2.3\nwindows: [\n    {\n        label: main\n        url: /index.html\n    }\n]\nexternal: {\n    connect: []\n    load: {\n        scripts: []\n        styles: []\n        images: []\n        fonts: []\n        media: []\n        frames: []\n    }\n}\npermissions: {\n    fs: {\n        read: []\n        write: []\n    }\n    cli: {\n        exec: []\n    }\n    net: {\n        http: []\n        socket: []\n    }\n    shell: {\n        openExternal: []\n    }\n    clipboard: {\n        read: false\n    }\n    shortcut: {\n        global: false\n    }\n    secrets: false\n    tray: false\n    window: {\n        create: false\n    }\n    app: {\n        env: []\n        autostart: false\n    }\n}\n');
  await writeFile(path.join(app, 'dist', 'index.html'), '<html></html>\n');
  const pkg = path.join(root, 'pkg.alef');
  success(await cli(['pack', app, '--out', pkg]));
  const runtime = path.join(root, 'runtime'); await writeFile(runtime, 'fake-runtime-bytes');
  const out = path.join(root, 'bundles'); await mkdir(out);
  const stdout = success(await cli(['bundle', pkg, '--runtime', runtime, '--platform', 'linux-x64',
    '--format', 'dir', '--out', out, '--runtime-version', '1.5.0']));
  const stemDir = path.join(out, 'Sample App-1.2.3-linux-x64');
  assert.equal(stdout, `${stemDir}\n`);
  assert.deepEqual(await readFile(path.join(stemDir, 'app', 'app.alef')), await readFile(pkg));
  assert.deepEqual(await readFile(path.join(stemDir, 'bin', 'alef')), Buffer.from('fake-runtime-bytes'));
}));
