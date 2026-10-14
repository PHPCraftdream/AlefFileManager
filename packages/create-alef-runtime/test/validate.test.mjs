// SPDX-License-Identifier: MIT OR Apache-2.0
import assert from 'node:assert/strict';
import test from 'node:test';
import { mkdtemp, readdir, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { generate } from '../src/generate.mjs';
import { validateDirectory, validateId, validateName, validateOptions } from '../src/validate.mjs';

function refuses(fn, pattern) {
  assert.throws(fn, error => pattern.test(error.message), `expected refusal matching ${pattern}`);
}

test('invalid_ids_are_refused_with_a_clear_message', () => {
  for (const id of ['a..b', 'a.', '.a', '', 'a b', 'приложение', 'a/b']) {
    refuses(() => validateId(id), /ID must contain ASCII/);
  }
});

test('invalid_display_names_are_refused_with_a_clear_message', () => {
  for (const name of ['say "hi"', "it's", '{{NAME}}', 'line\nbreak', 'tab\there']) {
    refuses(() => validateName(name), /Display name must contain only/);
  }
  assert.doesNotThrow(() => validateName('Приложение (v2)!'));
});

test('invalid_bundler_is_refused', () => {
  refuses(() => validateOptions({ directory: 'app', bundler: 'webpack' }), /Bundler must be rsbuild or vite/);
  refuses(() => validateOptions({ directory: 'app', bundler: '' }), /Bundler must be rsbuild or vite/);
});

test('invalid_directories_are_refused_before_any_directory_is_created', async () => {
  const bad = ['sub/dir', '..', '.', '../x', ['C', ':', '/x'].join(''), 'a\\b', 'dir.', 'dir ', 'con', 'NUL.txt', '', 'x'.repeat(256)];
  for (const directory of bad) {
    const cwd = await mkdtemp(path.join(tmpdir(), 'create-alef-refuse-'));
    try {
      await assert.rejects(() => generate({ directory, name: 'App', id: 'com.example.app', bundler: 'rsbuild' }, cwd),
        /Directory must be a Windows-portable/);
      assert.deepEqual(await readdir(cwd), [], `nothing may be created for ${JSON.stringify(directory)}`);
    } finally { await rm(cwd, { recursive: true, force: true }); }
  }
  for (const directory of bad) refuses(() => validateDirectory(directory), /Directory must be a Windows-portable/);
});

test('directory_containing_dotdot_mid_name_is_refused_and_creates_nothing', async () => {
  const cwd = await mkdtemp(path.join(tmpdir(), 'create-alef-dotdot-'));
  try {
    await assert.rejects(() => generate({ directory: 'dir..x', name: 'App', id: 'com.example.app', bundler: 'rsbuild' }, cwd),
      /Directory must be a Windows-portable/);
    assert.deepEqual(await readdir(cwd), [], 'a mid-name .. must be refused before any directory is created');
  } finally { await rm(cwd, { recursive: true, force: true }); }
});
