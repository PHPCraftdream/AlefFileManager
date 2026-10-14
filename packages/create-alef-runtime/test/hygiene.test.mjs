// SPDX-License-Identifier: MIT OR Apache-2.0
import assert from 'node:assert/strict';
import test from 'node:test';
import { exec } from 'node:child_process';
import { mkdir, mkdtemp, readFile, readdir, rm, realpath, symlink, writeFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { fileURLToPath } from 'node:url';
import { cli } from '../src/cli.mjs';
import { expectFailure, expectSuccess, runCli } from './helpers.mjs';

const packageRoot = fileURLToPath(new URL('../', import.meta.url));

test('package_contains_no_private_or_machine_specific_paths', async () => {
  const offenders = [];
  async function walk(dir) {
    for (const entry of await readdir(dir, { withFileTypes: true })) {
      const file = path.join(dir, entry.name);
      if (entry.isDirectory()) { if (entry.name !== 'node_modules') await walk(file); continue; }
      const text = await readFile(file, 'utf8').catch(() => '');
      if (text) {
        // Drive letters anywhere (not part of a scheme like http://), and path-shaped Windows machine directories.
        if (/(?<![A-Za-z])[A-Za-z]:[\\/]/.test(text)) offenders.push(`${file}: drive letter path`);
        if (/\b(?:users|appdata|temp)[\\/]/i.test(text) || /\bcomputer\b/i.test(text)) offenders.push(`${file}: machine-specific path`);
      }
    }
  }
  await walk(packageRoot);
  assert.deepEqual(offenders, []);
});

test('generation_succeeds_through_a_symlinked_parent_and_lands_in_the_real_directory', async t => {
  const real = await mkdtemp(path.join(tmpdir(), 'create-alef-real-'));
  const parent = path.join(real, 'link');
  try {
    await symlink(real, parent, 'dir');
    const result = await cli(['--yes', 'appsym'], { cwd: parent });
    const resolved = await realpath(path.join(real, 'appsym'));
    assert.equal(result.target, resolved, 'target must be reported as the real path');
    assert.ok(await readFile(path.join(real, 'appsym', 'package.json'), 'utf8'));
  } catch (error) {
    if (error.code === 'EPERM') { t.skip('symlink creation requires privileges on this machine'); return; }
    throw error;
  } finally { await rm(real, { recursive: true, force: true }); }
});

test('symlinked_target_directory_is_refused_whether_empty_or_not', async t => {
  const root = await mkdtemp(path.join(tmpdir(), 'create-alef-sym-target-'));
  try {
    for (const [kind, populate] of [['empty', false], ['non-empty', true]]) {
      const real = path.join(root, `real-${kind}`);
      await mkdir(real);
      if (populate) await writeFile(path.join(real, 'keep.txt'), 'keep');
      const link = path.join(root, `link-${kind}`);
      try { await symlink(real, link, 'dir'); }
      catch (error) { if (error.code === 'EPERM') { t.skip('symlink creation requires privileges on this machine'); return; } throw error; }
      const before = await readdir(real);
      expectFailure(await runCli(['--yes', `link-${kind}`], root), /symlink/);
      assert.deepEqual(await readdir(real), before, `nothing may be written through the ${kind} symlinked target`);
    }
  } finally { await rm(root, { recursive: true, force: true }); }
});

test('generation_succeeds_through_a_windows_8_3_short_parent_path', async t => {
  const real = await mkdtemp(path.join(tmpdir(), 'create-alef-short-name-parent-'));
  try {
    const short = await new Promise((resolve) => {
      exec('cmd.exe /c for %I in (.) do @echo %~sI', { cwd: real, timeout: 5000 },
        (error, stdout) => resolve(error ? undefined : stdout.trim()));
    });
    if (!short || short === real || !/[A-Z]{1,2}:[\\/]/.test(short) || short.includes('create-alef-short-name-parent-')) {
      t.skip('8.3 short names are unavailable on this volume');
      return;
    }
    const output = expectSuccess(await runCli(['--yes', 'appshort'], short));
    assert.match(output, /Created/);
    assert.ok(await readFile(path.join(real, 'appshort', 'alef.ktav'), 'utf8'), 'files must land in the real directory');
  } finally { await rm(real, { recursive: true, force: true }); }
});

test('rsbuild_index_html_strips_the_module_script_tag_and_vite_keeps_it', async () => {
  const root = await mkdtemp(path.join(tmpdir(), 'create-alef-html-'));
  try {
    for (const bundler of ['rsbuild', 'vite']) {
      await cli(['--yes', '--bundler', bundler, 'apphtml'], { cwd: root });
      const html = await readFile(path.join(root, 'apphtml', 'index.html'), 'utf8');
      if (bundler === 'rsbuild') assert.doesNotMatch(html, /<script type="module" src="\/src\/main\.js">/);
      else assert.match(html, /<script type="module" src="\/src\/main\.js">/);
      await rm(path.join(root, 'apphtml'), { recursive: true, force: true });
    }
  } finally { await rm(root, { recursive: true, force: true }); }
});
