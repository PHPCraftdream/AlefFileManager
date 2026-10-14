// node --test scripts/release/plan.test.mjs: the release plan's pure logic, the affected.json data and the release workflow's shape.
import assert from 'node:assert/strict';
import { readFileSync, existsSync } from 'node:fs';
import { mkdtemp, rm, mkdir, writeFile, readFile } from 'node:fs/promises';
import { tmpdir } from 'node:os';
import path from 'node:path';
import test from 'node:test';
import { fileURLToPath } from 'node:url';
import { checkVersions, collectPackages, planRelease, platformPackageNames } from './plan.mjs';
import { copyInto, filesToCopy } from './publish-package.mjs';

const PLATFORMS = [
  { os: 'windows-latest', name: 'windows-x64', target: 'x86_64-pc-windows-msvc', bin: 'alef.exe' },
  { os: 'macos-14', name: 'macos-arm64', target: 'aarch64-apple-darwin', bin: 'alef' },
  { os: 'macos-15-intel', name: 'macos-x64', target: 'x86_64-apple-darwin', bin: 'alef' },
  { os: 'ubuntu-24.04', name: 'linux-x64', target: 'x86_64-unknown-linux-gnu', bin: 'alef' },
];
const PLATFORM_NAMES = platformPackageNames(PLATFORMS);
const PACKAGES = [
  {
    name: '@alef-runtime/runtime', version: '0.1.0',
    optionalDependencies: Object.fromEntries(PLATFORM_NAMES.map(name => [name, 'workspace-version-placeholder'])),
  },
  { name: '@alef-runtime/api', version: '0.1.0' },
  { name: '@alef-runtime/mcp', version: '0.1.0' },
  { name: '@alef-runtime/tools', version: '0.1.0' },
  { name: 'create-alef-runtime', version: '0.1.0' },
];

const data = JSON.parse(readFileSync(new URL('../ci/affected.json', import.meta.url), 'utf8'));

test('the_versions_of_all_publishable_packages_are_one_consistent_version', () => {
  const result = planRelease({ platforms: PLATFORMS, packages: PACKAGES, tag: 'v0.1.0', real: true });
  assert.equal(result.version, '0.1.0');
});

test('a_drifted_package_fails_with_its_name_in_the_message', () => {
  // @alef-runtime/tools sorts after the other @alef-runtime packages, so it is the one reported as drifted
  const drifted = PACKAGES.map(pkg => pkg.name === '@alef-runtime/tools' ? { ...pkg, version: '0.2.0' } : pkg);
  assert.throws(
    () => planRelease({ platforms: PLATFORMS, packages: drifted, tag: 'v0.1.0', real: true }),
    error => /@alef-runtime\/tools has 0\.2\.0, expected 0\.1\.0/.test(error.message),
  );
});

test('a_tag_that_does_not_match_the_version_fails_in_real_mode', () => {
  assert.throws(
    () => planRelease({ platforms: PLATFORMS, packages: PACKAGES, tag: 'v0.2.0', real: true }),
    error => error.message.includes('tag v0.2.0') && error.message.includes('0.1.0'),
  );
});

test('a_mismatching_tag_is_ignored_in_dry_run', () => {
  const result = planRelease({ platforms: PLATFORMS, packages: PACKAGES, tag: 'v9.9.9', real: false });
  assert.equal(result.version, '0.1.0');
});

test('the_publish_order_puts_platform_packages_first_then_the_meta_and_tooling_packages', () => {
  const result = planRelease({ platforms: PLATFORMS, packages: PACKAGES, tag: 'v0.1.0', real: true });
  assert.deepEqual(result.order, [
    ...PLATFORM_NAMES,
    '@alef-runtime/runtime', '@alef-runtime/api', '@alef-runtime/mcp', '@alef-runtime/tools', 'create-alef-runtime',
  ]);
});

test('platform_package_names_derive_from_the_platforms_table', () => {
  assert.deepEqual(platformPackageNames(data.platforms), [
    '@alef-runtime/runtime-win32-x64', '@alef-runtime/runtime-darwin-arm64',
    '@alef-runtime/runtime-darwin-x64', '@alef-runtime/runtime-linux-x64',
  ]);
  assert.throws(
    () => platformPackageNames([{ os: 'plan9', name: 'plan9-ict', target: 'x', bin: 'alef' }]),
    /no platform suffix for CI name: plan9-ict/,
  );
});

test('every_platform_of_the_matrix_has_a_suffix_and_the_real_table_plans_clean', () => {
  const result = planRelease({ platforms: data.platforms, packages: PACKAGES, tag: 'v0.1.0', real: true });
  assert.deepEqual(result.matrix.include.map(p => p.name), data.platforms.map(p => p.name));
});

test('a_tail_package_missing_from_packages_fails_with_its_name', () => {
  const missing = PACKAGES.filter(pkg => pkg.name !== '@alef-runtime/api');
  assert.throws(
    () => planRelease({ platforms: PLATFORMS, packages: missing, tag: 'v0.1.0', real: true }),
    /@alef-runtime\/api is not in packages/,
  );
});

const workflowUrl = new URL('../../.github/workflows/release.yml', import.meta.url);
// Top-level jobs are the two-space-indented keys under `jobs:`; slice out one job's block.
function jobBlock(yaml, name) {
  const start = yaml.indexOf(`  ${name}:\n`);
  assert.notEqual(start, -1, `job ${name} not found`);
  const rest = yaml.slice(start + 1);
  const next = /^  [A-Za-z][\w-]*:\n/m.exec(rest);
  return rest.slice(0, next ? next.index + 1 : rest.length);
}
// Tests must not depend on checkout line endings, and comments must never satisfy an assertion.
const yaml = readFileSync(workflowUrl, 'utf8').replaceAll('\r\n', '\n')
  .split('\n').map(line => line.replace(/(^|\s)#.*$/, '').trimEnd()).join('\n');

const planBlock = () => jobBlock(yaml, 'plan');
const buildBlock = () => jobBlock(yaml, 'build');
const publishBlock = () => jobBlock(yaml, 'npm-publish');
const releaseBlock = () => jobBlock(yaml, 'github-release');

test('every_action_of_the_release_workflow_is_pinned_to_a_full_commit_sha', () => {
  for (const line of yaml.split('\n')) {
    const m = line.match(/^\s*(?:-\s+)?uses:\s*(\S+)/);
    if (!m) continue;
    const ref = m[1].split('@')[1];
    assert.match(ref ?? '', /^[0-9a-f]{40}$/, `unpinned action ref: ${m[1]}`);
  }
});

test('the_release_workflow_has_no_pull_request_target_trigger', () => {
  assert.doesNotMatch(yaml, /pull_request_target/);
});

test('the_publishing_job_declares_the_protected_release_environment', () => {
  assert.match(publishBlock(), /environment: release/);
  // plan and build must stay environment-free, so moving the string around cannot satisfy this
  assert.doesNotMatch(planBlock(), /environment:/);
  assert.doesNotMatch(buildBlock(), /environment:/);
});

test('the_build_matrix_comes_from_the_plan_and_has_no_include_of_its_own', () => {
  assert.match(yaml, /matrix:\s*\$\{\{\s*fromJSON\(needs\.plan\.outputs\.matrix\)\s*\}\}/);
  assert.doesNotMatch(yaml, /^\s+include:\s*$/m);
});

test('real_mode_fails_before_any_publishing_without_the_npm_token', () => {
  const block = planBlock();
  // the guard step exists only in real mode and refuses to continue without the token
  const step = block.slice(block.indexOf('Real mode needs NPM_TOKEN'));
  assert.match(step, /if: steps\.plan\.outputs\.real == 'true'/);
  assert.match(step, /\[ -z "\$NPM_TOKEN" \]/);
  assert.match(step, /exit 1/);
});

test('the_dry_run_publish_step_sees_no_secret_and_the_real_one_uses_the_token', () => {
  const block = publishBlock();
  const dryStart = block.indexOf('Publish (dry run)');
  const realStart = block.indexOf('Publish (real)');
  assert.notEqual(dryStart, -1);
  assert.notEqual(realStart, -1);
  assert.ok(dryStart < realStart);
  const dry = block.slice(dryStart, realStart);
  const real = block.slice(realStart);
  assert.match(dry, /if: needs\.plan\.outputs\.real != 'true'/);
  assert.match(real, /if: needs\.plan\.outputs\.real == 'true'/);
  assert.doesNotMatch(dry, /secrets\./);
  assert.match(real, /NODE_AUTH_TOKEN: \$\{\{ secrets\.NPM_TOKEN \}\}/);
  // both dry-run publish invocations must carry the flag; the real one must not
  const invocations = dry.match(/publish-package\.mjs[^\n]+/g);
  assert.equal(invocations?.length, 2);
  for (const invocation of invocations) assert.ok(invocation.trimEnd().endsWith('--dry-run'));
  for (const invocation of real.matchAll(/publish-package\.mjs[^\n]+/g)) {
    assert.doesNotMatch(invocation[0], /--dry-run/);
  }
});

function permissionsOf(block) {
  const m = /permissions:\n((?:\s+\S.*\n)+)/.exec(block);
  assert.ok(m, 'no permissions mapping found');
  return m[1].split('\n').map(line => line.trim()).filter(Boolean);
}
test('both_artifact_downloading_jobs_declare_actions_read_permissions', () => {
  assert.ok(permissionsOf(publishBlock()).includes('actions: read'));
  assert.ok(permissionsOf(releaseBlock()).includes('actions: read'));
});

test('the_binaries_are_extracted_from_the_archives_before_the_generator_runs', () => {
  const block = publishBlock();
  // both archive formats are extracted; removing either command must fail this test
  assert.match(block, /unzip -o /);
  assert.match(block, /tar -xzf /);
  const extract = Math.min(block.search(/unzip -o /), block.search(/tar -xzf /));
  const generate = block.indexOf('make-platform-package.mjs');
  assert.ok(extract < generate);
});

test('a_manifest_with_files_stages_exactly_those_entries', () => {
  assert.deepEqual(filesToCopy({ files: ['src', 'bin'] }), ['src', 'bin']);
  assert.equal(filesToCopy({ name: 'create-alef-runtime' }), null);
});

test('staging_a_manifest_without_files_copies_bin_src_and_templates_but_not_node_modules', async () => {
  const root = await mkdtemp(path.join(tmpdir(), 'alef-stage-src-'));
  const staging = await mkdtemp(path.join(tmpdir(), 'alef-stage-out-'));
  try {
    await mkdir(path.join(root, 'bin'), { recursive: true });
    await mkdir(path.join(root, 'src'), { recursive: true });
    await mkdir(path.join(root, 'templates', 'base'), { recursive: true });
    await mkdir(path.join(root, 'node_modules', 'junk'), { recursive: true });
    await writeFile(path.join(root, 'package.json'), JSON.stringify({ name: 'fixture', version: '0.0.0' }));
    await writeFile(path.join(root, 'bin', 'run.mjs'), 'export {};\n');
    await writeFile(path.join(root, 'src', 'index.mjs'), 'export {};\n');
    await writeFile(path.join(root, 'templates', 'base', 't.txt'), 'x\n');
    await writeFile(path.join(root, 'node_modules', 'junk', 'j.mjs'), 'export {};\n');
    const manifest = JSON.parse(await readFile(path.join(root, 'package.json'), 'utf8'));
    await rm(staging, { recursive: true, force: true });
    await mkdir(staging, { recursive: true });
    copyInto(root, staging, manifest);
    assert.ok(existsSync(path.join(staging, 'bin', 'run.mjs')));
    assert.ok(existsSync(path.join(staging, 'src', 'index.mjs')));
    assert.ok(existsSync(path.join(staging, 'templates', 'base', 't.txt')));
    assert.ok(!existsSync(path.join(staging, 'node_modules')));
  } finally {
    await rm(root, { recursive: true, force: true });
    await rm(staging, { recursive: true, force: true });
  }
});

test('the_release_build_uses_the_dist_profile_and_not_the_release_profile', () => {
  const block = buildBlock();
  assert.match(block, /--profile dist/);
  assert.doesNotMatch(block, /--release\b/);
});

test('the_dist_binary_is_scanned_for_test_hooks_and_smoke_checked_from_dist', () => {
  const block = buildBlock();
  // the published binary is the one scanned: the exact dist path, per-OS bin substitution
  assert.match(block, /check-no-hooks\.mjs backend\/target\/dist\/\$\{\{ matrix\.bin \}\}/);
  assert.match(block, /backend\/target\/dist\/\$\{\{ matrix\.bin \}\} --help/);
});

test('the_size_report_step_appends_the_summary_and_writes_the_json_artifact', () => {
  const block = buildBlock();
  // the statements themselves, not just identifiers: removing either call must fail
  assert.match(block, /appendFileSync\(process\.env\.GITHUB_STEP_SUMMARY/);
  assert.match(block, /writeFileSync\('size-' \+ platform \+ '\.json'/);
  assert.match(block, /size-\*\.json/);
});

test('the_dist_profile_strips_and_uses_thin_lto_while_plain_release_stays_untouched', () => {
  const toml = readFileSync(new URL('../../backend/Cargo.toml', import.meta.url), 'utf8').replaceAll('\r\n', '\n');
  // Scoped to the [profile.dist] section only: a regex on the whole file cannot tell which profile a key belongs to.
  const section = (name) => {
    const start = toml.indexOf(`[${name}]`);
    assert.notEqual(start, -1, `missing [${name}] section`);
    const rest = toml.slice(start + 1);
    const next = /^\[/m.exec(rest);
    return rest.slice(0, next ? next.index : rest.length);
  };
  const dist = section('profile.dist');
  assert.match(dist, /^inherits = "release"$/m);
  assert.match(dist, /^strip = "symbols"$/m);
  assert.match(dist, /^lto = "thin"$/m);
  // If a plain [profile.release] section ever appears, it must not duplicate the dist settings (CI timings unchanged).
  if (toml.includes('[profile.release]')) {
    const release = section('profile.release');
    assert.doesNotMatch(release, /^lto =/m);
    assert.doesNotMatch(release, /^strip =/m);
  }
});

test('the_real_packages_all_declare_one_version', () => {
  const root = fileURLToPath(new URL('../../', import.meta.url));
  assert.equal(checkVersions(collectPackages(root)), '0.1.0');
});
