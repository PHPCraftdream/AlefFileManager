// SPDX-License-Identifier: MIT OR Apache-2.0
import assert from 'node:assert/strict';
import test from 'node:test';
import { PassThrough } from 'node:stream';
import { mkdir, readFile, readdir, writeFile, rm } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import { mkdtemp } from 'node:fs/promises';
import { cli } from '../src/cli.mjs';
import { render } from '../src/generate.mjs';
import { expectSuccess, expectFailure, runCli } from './helpers.mjs';

const values = { id: 'com.example.app', name: 'App', package: 'app' };
async function scratch(run) {
  const root = await mkdtemp(path.join(tmpdir(), 'create-alef-cli-'));
  try { await run(root); } finally { await rm(root, { recursive: true, force: true }); }
}

test('non_empty_existing_target_is_refused_and_nothing_is_written', { timeout: 30000 }, async () => {
  await scratch(async root => {
    const target = path.join(root, 'app');
    await mkdir(target);
    await writeFile(path.join(target, 'keep.txt'), 'keep');
    await assert.rejects(() => cli(['--yes', 'app'], { cwd: root }), /not empty/);
    assert.deepEqual(await readdir(target), ['keep.txt']);
  });
});

test('placeholder_injection_through_values_is_refused', { timeout: 30000 }, async () => {
  await scratch(async root => {
    await assert.rejects(() => cli(['--yes', '--name', 'A {{ID}} B', 'app'], { cwd: root }),
      /Display name must contain only/);
    assert.throws(() => render('hello {{NAME}}', { ...values, name: 'x {{ID}}' }), /Invalid value for template token NAME/);
    assert.throws(() => render('a{{X}}b', values), /Unknown template token/);
    assert.throws(() => render('{{X}}{{', values), /Unknown template token|Unresolved template placeholder/);
    assert.throws(() => render('text with {{ stray', values), /Unresolved template placeholder/);
  });
});

test('render_refuses_unknown_tokens_and_newline_names_are_refused_by_the_cli', { timeout: 30000 }, async () => {
  // render() refuses hostile values itself; the CLI-level whitelist rejects them earlier, with its own message.
  assert.throws(() => render('value {{NOPE}}', values), /Unknown template token: NOPE/);
  assert.equal(render('id {{ID}} pkg {{PACKAGE}} ver {{API}} {{TOOLS}} {{RSBUILD}} {{VITE}}', values),
    'id com.example.app pkg app ver ^0.1.0 ^0.1.0 2.2.9 ^6.0.0');
  await assert.rejects(() => cli(['--yes', '--name', 'bad\nname', 'app'], { cwd: process.cwd() }),
    /Display name must contain only/);
});

test('render_refuses_hostile_values_per_token', () => {
  // Each token's value is validated against its place's shape before substitution.
  assert.throws(() => render('id {{ID}}', { ...values, id: 'a\nb' }), /Invalid value for template token ID/);
  assert.throws(() => render('{{NAME}}', { ...values, name: 'A\nB' }), /Invalid value for template token NAME/);
  assert.throws(() => render('{{NAME}}', { ...values, name: 'A {{ID}} B' }), /Invalid value for template token NAME/);
  assert.throws(() => render('{{PACKAGE}}', { ...values, package: 'a\nb' }), /Invalid value for template token PACKAGE/);
  assert.throws(() => render('{{PACKAGE}}', { ...values, package: 'My.App' }), /Invalid value for template token PACKAGE/);
  assert.equal(render('id {{ID}}', values), 'id com.example.app');
});

test('render_refuses_missing_and_non_string_values', () => {
  for (const bad of [undefined, null, 42, {}, ['a']]) {
    assert.throws(() => render('{{ID}}', { ...values, id: bad }), /a string is required/, String(bad));
  }
  assert.throws(() => render('{{PACKAGE}}', { id: 'a.b', name: 'App' }), /a string is required/);
});

test('yes_flag_never_prompts_and_succeeds_with_piped_stdin', async () => {
  await scratch(async root => {
    const output = expectSuccess(await runCli(['--yes', 'appyes'], root, { input: 'garbage that must be ignored' }));
    assert.doesNotMatch(output, /\[[^\]]*\]:/, '--yes must not write any prompt marker');
    assert.ok(await readFile(path.join(root, 'appyes', 'package.json'), 'utf8'));
  });
});

test('interactive_piped_lines_succeed_and_empty_lines_take_defaults', async () => {
  await scratch(async root => {
    expectSuccess(await runCli(['appint'], root, { input: 'My Interactive App\ncom.example.interactive\nvite\n' }));
    const pkg = JSON.parse(await readFile(path.join(root, 'appint', 'package.json'), 'utf8'));
    assert.equal(pkg.name, 'appint');
  });
  await scratch(async root => {
    expectSuccess(await runCli([], root, { input: '\n\n\n\n' }));
    assert.ok(await readFile(path.join(root, 'my-alef-app', 'package.json'), 'utf8'), 'defaults must apply');
  });
});

test('eof_mid_prompt_fails_with_a_clear_message', { timeout: 30000 }, async () => {
  await scratch(async root => {
    const input = new PassThrough();
    input.end('appdir\n');
    await assert.rejects(() => cli(['appdir'], { input, output: new PassThrough(), cwd: root }),
      /Input ended \(EOF\) while asking for/);
    await assert.rejects(() => readdir(path.join(root, 'appdir')), { code: 'ENOENT' });
  });
});

test('prompt_timeout_rejects_with_timeout_message_and_creates_nothing', { timeout: 30000 }, async () => {
  await scratch(async root => {
    const input = new PassThrough(); // Never emits a line, never closes.
    await assert.rejects(() => cli([], { input, output: new PassThrough(), cwd: root, timeoutMs: 50 }),
      /Prompt timed out after/);
    assert.deepEqual(await readdir(root), []);
  });
});

test('run_leaves_no_files_outside_the_target_directory', { timeout: 30000 }, async () => {
  await scratch(async root => {
    const before = (await readdir(root, { recursive: true })).sort();
    await cli(['--yes', 'apoutside'], { cwd: root });
    const after = (await readdir(root, { recursive: true })).map(name => name.replaceAll(path.sep, '/')).sort();
    assert.deepEqual(after, [...before.map(name => name.replaceAll(path.sep, '/')), 'apoutside', 'apoutside/.gitignore', 'apoutside/README.md',
      'apoutside/alef.ktav', 'apoutside/icon.png', 'apoutside/index.html', 'apoutside/package.json',
      'apoutside/rsbuild.config.mjs', 'apoutside/src', 'apoutside/src/main.js', 'apoutside/src/permissions.js'].sort());
  });
});

test('cli_child_refuses_invalid_input_with_clear_stderr', async () => {
  await scratch(async root => {
    expectFailure(await runCli(['--yes', 'sub/dir'], root), /Windows-portable/);
    expectFailure(await runCli(['--yes', '--bundler', 'webpack', 'app'], root), /Bundler must be/);
  });
});
