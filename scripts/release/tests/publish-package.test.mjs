// node --test scripts/release/tests/publish-package.test.mjs: the `files` staging of publish-package.mjs.
import assert from 'node:assert/strict';
import { existsSync, mkdirSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { copyInto, stagedManifest } from '../publish-package.mjs';

const manifest = name => ({ name, files: ['dist', 'README.md'] });

test('copyInto_stages_exactly_the_files_entries_directories_included_and_nothing_else', () => {
  const root = mkdtempSync(path.join(tmpdir(), 'alef-publish-'));
  try {
    mkdirSync(path.join(root, 'dist', 'src'), { recursive: true });
    writeFileSync(path.join(root, 'dist', 'src', 'index.js'), 'export {};\n');
    writeFileSync(path.join(root, 'README.md'), '# x\n');
    writeFileSync(path.join(root, 'secret.txt'), 'do not ship\n');
    const staging = path.join(root, 'staging');
    mkdirSync(staging);
    copyInto(root, staging, manifest('x'));
    assert.equal(readFileSync(path.join(staging, 'dist', 'src', 'index.js'), 'utf8'), 'export {};\n');
    assert.equal(readFileSync(path.join(staging, 'README.md'), 'utf8'), '# x\n');
    assert.ok(!existsSync(path.join(staging, 'secret.txt')), 'files outside "files" must not be staged');
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test('copyInto_refuses_to_stage_a_package_whose_files_entry_is_missing', () => {
  const root = mkdtempSync(path.join(tmpdir(), 'alef-publish-'));
  try {
    // dist/ is a build artifact: staging without it would publish a manifest without an implementation.
    assert.throws(() => copyInto(root, path.join(root, 'staging'), manifest('x')), /"files" entry "dist" is missing/);
  } finally {
    rmSync(root, { recursive: true, force: true });
  }
});

test('the_staged_manifest_of_a_built_files_package_drops_the_build_lifecycle_that_needs_sources', () => {
  const staged = stagedManifest({ name: 'x', private: true, version: '0.0.0', files: ['dist'],
    scripts: { prepack: 'node build.mjs', prepare: 'node build.mjs', test: 'node --test' } }, '1.2.3');
  assert.equal(staged.version, '1.2.3');
  assert.equal(staged.private, undefined);
  assert.deepEqual(staged.scripts, { test: 'node --test' });
  const only = stagedManifest({ name: 'y', files: ['dist'], scripts: { prepack: 'node build.mjs' } }, '1.0.0');
  assert.equal(only.scripts, undefined, 'an emptied scripts object is removed');
});

test('a_package_staged_whole_keeps_its_scripts_because_its_sources_are_staged_too', () => {
  const staged = stagedManifest({ name: 'z', scripts: { prepare: 'node gen.mjs' } }, '1.0.0');
  assert.deepEqual(staged.scripts, { prepare: 'node gen.mjs' });
});

test('the_runtime_placeholder_of_optional_dependencies_resolves_to_the_release_version', () => {
  const staged = stagedManifest({ name: 'm', optionalDependencies: { a: 'workspace-version-placeholder', b: '9.9.9' } }, '2.0.0');
  assert.deepEqual(staged.optionalDependencies, { a: '2.0.0', b: '9.9.9' });
});
