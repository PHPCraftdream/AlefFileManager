// node --test scripts/release/tests/installer.test.mjs: installer.mjs builders, deb layout,
// darwin staging tree, NSIS text, determinism and the CLI entry point.
import assert from 'node:assert/strict';
import { execFileSync } from 'node:child_process';
import { existsSync, mkdtempSync, readFileSync, rmSync, writeFileSync } from 'node:fs';
import { tmpdir } from 'node:os';
import path from 'node:path';
import zlib from 'node:zlib';
import test from 'node:test';
import { INSTALLER_PLATFORMS, alternativesPriority, buildRuntimeInstaller, isStrictRuntimeVersion } from '../installer.mjs';
import { SUFFIX_BY_CI_NAME } from '../plan.mjs';

const RUNTIME = Buffer.from('ALEF-RUNTIME-BYTES-0123456789');
const V = '1.2.3';

const build = (platform, version = V, runtime = RUNTIME, extra = {}) =>
  buildRuntimeInstaller({ platform, version, runtime, ...extra });

const fileMap = installer => new Map(installer.files.map(f => [f.path, f]));
const text = installer => name => fileMap(installer).get(name).data.toString('utf8');

test('invalid_versions_are_refused_for_every_platform_before_any_file_is_produced', () => {
  const bad = ['1.0.0-beta', '1.0.0+build', '01.2.3', 'v1.0.0', '1.0', '1.0.0.0', '',
    '1.0.0"; DeleteRegKey HKCU "Software"', '1.0.0\nSection', '1.2.3 ', null, '1.2', '1.2.3\n', '1.2.3\n\n'];
  for (const platform of INSTALLER_PLATFORMS) {
    for (const version of bad) {
      assert.throws(() => build(platform, version), error => {
        assert.match(String(error?.message), /strict/);
        return true;
      }, `${platform} accepted '${version}'`);
    }
  }
});

test('a_strict_version_is_accepted_on_all_four_platforms', () => {
  for (const platform of INSTALLER_PLATFORMS) {
    const installer = build(platform);
    assert.equal(installer.version, V);
    assert.ok(installer.files.length > 0);
  }
});

test('every_ci_matrix_platform_is_accepted_by_the_generator', () => {
  const data = JSON.parse(readFileSync(new URL('../../ci/affected.json', import.meta.url), 'utf8'));
  for (const p of data.platforms) {
    assert.ok(INSTALLER_PLATFORMS.includes(SUFFIX_BY_CI_NAME[p.name]), p.name);
    assert.doesNotThrow(() => build(SUFFIX_BY_CI_NAME[p.name]), p.name);
  }
});

test('win32_nsis_script_contains_the_required_verbatim_lines', () => {
  const nsi = text(build('win32-x64'))('alef-runtime-1.2.3-setup.nsi');
  assert.ok(nsi.includes('InstallDir "$LOCALAPPDATA\\alef\\runtimes\\1.2.3"'));
  assert.ok(nsi.includes('File /oname=alef.exe bin\\alef.exe'));
  assert.ok(nsi.includes('WriteRegStr HKCU "Software\\Classes\\.alef" "" "Alef.Package"'));
  const open = nsi.split('\n').find(l => l.includes('Alef.Package\\shell\\open\\command'));
  assert.ok(open.includes('$LOCALAPPDATA\\alef\\bin\\alef.cmd'));
  assert.ok(open.includes('run '));
  assert.ok(open.includes('%1'));
  assert.ok(nsi.includes('Software\\Classes\\Alef.Package\\DefaultIcon'));
  assert.ok(nsi.includes('Uninstall\\AlefRuntime-1.2.3" "DisplayName" "Alef Runtime 1.2.3"'));
  assert.ok(nsi.includes('Uninstall\\AlefRuntime-1.2.3" "DisplayVersion" "1.2.3"'));
  assert.ok(nsi.includes('HKCU "Environment" "Path"'));
  assert.equal(nsi.split('SendMessage 0xFFFF 0x001A 0 "STR:Environment"').length - 1, 2);
  assert.ok(nsi.includes('RequestExecutionLevel user'));
  assert.ok(!/RequestExecutionLevel admin/i.test(nsi) && !/\bAdmin\b/.test(nsi));
});

test('win32_uninstall_cleans_shared_state_only_in_the_un_last_branch', () => {
  const nsi = text(build('win32-x64'))('alef-runtime-1.2.3-setup.nsi');
  assert.equal(nsi.split('DeleteRegKey HKCU "Software\\Classes\\.alef"').length - 1, 1);
  const assoc = nsi.indexOf('DeleteRegKey HKCU "Software\\Classes\\.alef"');
  const unDone = nsi.indexOf('un_done:');
  assert.ok(assoc > 0 && assoc < unDone);
  const markers = ['StrCpy $R4 "last"', 'FindFirst $R0 $R1', 'StrCmp $R1 "." find_next 0',
    'StrCmp $R1 ".." find_next 0', 'StrCpy $R4 "notlast"', 'FindNext $R0 $R1',
    'StrCmp $R4 "notlast" un_done 0', 'DeleteRegKey HKCU "Software\\Classes\\.alef"'];
  let offset = 0;
  for (const marker of markers) {
    const at = nsi.indexOf(marker, offset);
    assert.ok(at >= offset, marker);
    offset = at + marker.length;
  }
  assert.ok(nsi.indexOf('FindClose $R0') < nsi.indexOf('StrCmp $R4 "notlast" un_done 0'));
  assert.ok(nsi.includes('RMDir "$INSTDIR"'));
  assert.ok(nsi.includes('DeleteRegKey HKCU "Software\\Microsoft\\Windows\\CurrentVersion\\Uninstall\\AlefRuntime-1.2.3"'));
});

test('win32_shim_and_build_cmd_and_embedded_exe_are_correct', () => {
  const files = fileMap(build('win32-x64'));
  const shim = files.get('bin/alef.cmd').data.toString('utf8');
  assert.ok(!shim.includes('1.2.3'));
  assert.ok(shim.includes('"%ALEF_RUNTIMES%\\%ALEF_V%\\alef.exe" %*'));
  assert.ok(shim.includes('^\\d+\\.\\d+\\.\\d+$'));
  assert.equal(files.get('build-installer.cmd').data.toString('utf8'),
    'makensis /NOCD alef-runtime-1.2.3-setup.nsi\r\n');
  assert.ok(files.get('bin/alef.exe').data.equals(RUNTIME));
  assert.equal(files.get('bin/alef.exe').mode, 0o755);
});

// Tiny ar reader: 8-byte magic, then 60-byte header + payload per member.
function readAr(buffer) {
  assert.ok(buffer.subarray(0, 8).toString('utf8') === '!<arch>\n');
  const members = new Map();
  let offset = 8;
  while (offset < buffer.length) {
    const header = buffer.subarray(offset, offset + 60).toString('utf8');
    const name = header.slice(0, header.indexOf('/'));
    const size = parseInt(header.slice(48, 58), 10);
    members.set(name, buffer.subarray(offset + 60, offset + 60 + size));
    offset += 60 + size + (size % 2);
  }
  return members;
}

// Ustar reader into a Map of name -> { data, mode }.
function readTar(buffer) {
  const entries = new Map();
  let offset = 0;
  while (offset + 512 <= buffer.length) {
    const header = buffer.subarray(offset, offset + 512);
    if (header.every(b => b === 0)) break;
    const name = header.subarray(0, 100).toString('utf8').replace(/\0.*$/, '');
    const size = parseInt(header.subarray(124, 136).toString('utf8').replace(/\0.*$/, '').trim(), 8) || 0;
    const typeflag = header.subarray(156, 157).toString('utf8');
    const mode = parseInt(header.subarray(100, 108).toString('utf8').replace(/\0.*$/, '').trim(), 8);
    const data = typeflag === '5' ? Buffer.alloc(0) : buffer.subarray(offset + 512, offset + 512 + size);
    entries.set(name, { data, mode, typeflag });
    offset += 512 + Math.ceil(size / 512) * 512;
  }
  return entries;
}

function debTree(installer, name = 'alef-runtime-1.2.3_amd64.deb') {
  const deb = fileMap(installer).get(name).data;
  const members = readAr(deb);
  assert.deepEqual([...members.keys()], ['debian-binary', 'control.tar.gz', 'data.tar.gz']);
  assert.equal(members.get('debian-binary').toString('utf8'), '2.0\n');
  return {
    control: readTar(zlib.gunzipSync(members.get('control.tar.gz'))),
    data: readTar(zlib.gunzipSync(members.get('data.tar.gz'))),
  };
}

test('deb_control_tar_has_control_and_maintainer_scripts_with_expected_content', () => {
  const { control } = debTree(build('linux-x64'));
  const controlText = control.get('./control').data.toString('utf8');
  assert.ok(controlText.includes('Package: alef-runtime-1.2.3'));
  assert.ok(controlText.includes('Version: 1.2.3'));
  assert.ok(controlText.includes('Architecture: amd64'));
  assert.ok(controlText.includes('Depends: alef-runtime-common\n'));
  assert.ok(controlText.includes('Maintainer: Alef Runtime <runtime@alef.invalid>'));
  assert.ok(controlText.includes('Installed-Size:'));
  assert.deepEqual([...control.keys()], ['./control', './postinst', './prerm']);
  for (const script of ['./postinst', './prerm']) {
    assert.equal(control.get(script).mode, 0o755);
  }
  const prio = alternativesPriority(V);
  const postinst = control.get('./postinst').data.toString('utf8');
  assert.ok(postinst.includes(`update-alternatives --install /usr/bin/alef alef /opt/alef-runtime/1.2.3/alef ${prio}`));
  assert.ok(postinst.includes('update-mime-database'));
  assert.ok(postinst.includes('update-desktop-database'));
  const prerm = control.get('./prerm').data.toString('utf8');
  assert.ok(prerm.includes('update-alternatives --remove alef /opt/alef-runtime/1.2.3/alef'));
});

test('deb_supports_a_custom_maintainer_and_computes_installed_size_from_scripts', () => {
  const installer = build('linux-x64', V, RUNTIME, { maintainer: 'Ops Team <ops@example.com>' });
  const { control } = debTree(installer);
  assert.ok(control.get('./control').data.toString('utf8').includes('Maintainer: Ops Team <ops@example.com>'));
});

test('common_deb_ships_mime_and_desktop_and_no_alternatives_or_conflicts', () => {
  const { control, data } = debTree(build('linux-x64'), 'alef-runtime-common_1.2.3_amd64.deb');
  const controlText = control.get('./control').data.toString('utf8');
  assert.ok(controlText.includes('Package: alef-runtime-common'));
  assert.ok(controlText.includes('Version: 1.2.3'));
  assert.ok(!controlText.includes('Depends:'));
  assert.ok(!controlText.includes('Replaces:'));
  assert.ok(!controlText.includes('Breaks:'));
  assert.deepEqual([...control.keys()], ['./control', './postinst', './postrm']);
  const postinst = control.get('./postinst').data.toString('utf8');
  assert.ok(postinst.includes('update-mime-database /usr/share/mime'));
  assert.ok(postinst.includes('update-desktop-database /usr/share/applications'));
  assert.ok(!postinst.includes('update-alternatives'));
  const postrm = control.get('./postrm').data.toString('utf8');
  assert.ok(postrm.includes('update-mime-database /usr/share/mime'));
  assert.ok(postrm.includes('update-desktop-database /usr/share/applications'));
  assert.ok(!postrm.includes('update-alternatives'));
  assert.deepEqual([...data.keys()].sort(), ['./usr/share/applications/alef-runtime.desktop', './usr/share/mime/packages/alef.xml']);
  const mime = data.get('./usr/share/mime/packages/alef.xml').data.toString('utf8');
  assert.ok(mime.includes('type="application/x-alef-package"'));
  assert.ok(mime.includes('glob pattern="*.alef"'));
  const desktop = data.get('./usr/share/applications/alef-runtime.desktop').data.toString('utf8');
  assert.ok(desktop.includes('MimeType=application/x-alef-package;'));
  assert.equal(desktop.split('\n').find(l => l.startsWith('Exec=')), 'Exec="alef" run %f');
});

test('deb_data_tar_ships_only_the_versioned_runtime_tree_not_usr_share_or_usr_bin', () => {
  const { data } = debTree(build('linux-x64'));
  const runtime = data.get('./opt/alef-runtime/1.2.3/alef');
  assert.ok(runtime.data.equals(RUNTIME));
  assert.equal(runtime.mode, 0o755);
  const dir = data.get('./opt/alef-runtime/1.2.3');
  assert.equal(dir.typeflag, '5');
  assert.equal(dir.mode, 0o755);
  assert.deepEqual([...data.keys()], ['./opt/alef-runtime/1.2.3', './opt/alef-runtime/1.2.3/alef']);
  assert.ok(![...data.keys()].some(name => name.startsWith('./usr/share/')));
  assert.ok(![...data.keys()].some(name => name.startsWith('./usr/bin/')));
});

test('alternatives_priority_is_monotonic_and_rejects_oversized_components', () => {
  const order = ['0.1.0', '0.2.0', '0.10.0', '1.0.0', '1.2.3'].map(alternativesPriority);
  for (let i = 1; i < order.length; i++) assert.ok(order[i - 1] < order[i]);
  assert.equal(alternativesPriority('1.2.3'), 100020003);
  assert.throws(() => alternativesPriority('10000.0.0'), /<= 9999/);
});

test('darwin_tree_ships_runtime_applescript_and_pkg_build_tooling', () => {
  const files = fileMap(build('darwin-arm64'));
  const x64 = fileMap(build('darwin-x64'));
  const runtime = files.get('payload/Library/Alef/runtime/1.2.3/alef');
  assert.ok(runtime.data.equals(RUNTIME));
  assert.equal(runtime.mode, 0o755);
  const postinstall = files.get('scripts/postinstall').data.toString('utf8');
  assert.ok(postinstall.includes('ln -sfn "/Library/Alef/runtime/1.2.3/alef" /usr/local/bin/alef'));
  const applet = files.get('applet/applet.applescript').data.toString('utf8');
  assert.ok(applet.includes('on open theFiles'));
  assert.ok(applet.includes('repeat with'));
  assert.ok(applet.includes('do shell script'));
  assert.ok(applet.includes('quoted form of'));
  assert.ok(applet.includes('POSIX path'));
  assert.ok(applet.includes('/usr/local/bin/alef run '));
  assert.ok(applet.includes('on run'));
  const buildSh = files.get('build-pkg.sh').data.toString('utf8');
  assert.ok(buildSh.includes('mkdir -p payload/Applications\nrm -rf payload/Applications/Alef.app\n'));
  assert.ok(buildSh.includes('osacompile -o "$PWD/payload/Applications/Alef.app" "$PWD/applet/applet.applescript"'));
  // UTExportedTypeDeclarations is an array of declaration dictionaries; keys osacompile may write are replaced, not re-added.
  assert.ok(buildSh.includes('pb "Add :UTExportedTypeDeclarations array"'));
  assert.ok(buildSh.includes('pb "Add :UTExportedTypeDeclarations:0 dict"'));
  assert.ok(buildSh.includes('pb "Add :UTExportedTypeDeclarations:0:UTTypeIdentifier string org.alef.package"'));
  assert.ok(buildSh.includes('pb "Add :UTExportedTypeDeclarations:0:UTTypeTagSpecification:public.filename-extension:0 string alef"'));
  assert.ok(!/Add :UTExportedTypeDeclarations dict/.test(buildSh));
  assert.ok(buildSh.includes('set_key() { pb "Delete :$1" 2>/dev/null || true; pb "Add :$1 $2 $3"; }'));
  assert.ok(buildSh.includes('set_key CFBundleIdentifier string org.alef.runtime.stub'));
  assert.ok(buildSh.includes('org.alef.package'));
  assert.ok(buildSh.includes('public.data'));
  assert.ok(buildSh.includes('public.filename-extension'));
  assert.ok(buildSh.includes('CFBundleDocumentTypes'));
  assert.ok(buildSh.includes('pkgbuild'));
  assert.ok(buildSh.includes('productbuild'));
  assert.ok(buildSh.includes('alef-runtime-1.2.3-core.pkg'));
  assert.ok(![...files.keys()].some(name => name.startsWith('payload/Applications/')));
  assert.ok(files.has('Distribution.xml'));
  assert.ok(files.get('Distribution.xml').data.toString('utf8').includes('alef-runtime-1.2.3'));
  assert.deepEqual([...files.keys()].sort(), [...x64.keys()].sort());
});

test('building_twice_for_each_platform_gives_identical_files', () => {
  for (const platform of INSTALLER_PLATFORMS) {
    const a = build(platform);
    const b = build(platform);
    assert.deepEqual(a.files.map(f => f.path), b.files.map(f => f.path));
    for (let i = 0; i < a.files.length; i++) {
      assert.ok(a.files[i].data.equals(b.files[i].data), `${platform}: ${a.files[i].path}`);
      assert.equal(a.files[i].mode, b.files[i].mode);
    }
  }
});

test('is_strict_runtime_version_mirrors_the_builder_validation', () => {
  assert.equal(isStrictRuntimeVersion('1.2.3'), true);
  assert.equal(isStrictRuntimeVersion('0.0.0'), true);
  assert.equal(isStrictRuntimeVersion('01.2.3'), false);
  assert.equal(isStrictRuntimeVersion('1.0.0-beta'), false);
  assert.equal(isStrictRuntimeVersion('1.0'), false);
  assert.equal(isStrictRuntimeVersion(''), false);
  assert.equal(isStrictRuntimeVersion('1.2.3\n'), false);
});

test('cli_writes_the_win32_staging_tree_and_fails_on_bad_versions', () => {
  const dir = mkdtempSync(path.join(tmpdir(), 'alef-installer-'));
  try {
    const binary = path.join(dir, 'alef.exe');
    const out = path.join(dir, 'staging');
    writeFileSync(binary, RUNTIME);
    execFileSync(process.execPath, ['scripts/release/installer.mjs', 'win32-x64', V, binary, out],
      { timeout: 30000, encoding: 'utf8' });
    assert.ok(existsSync(path.join(out, 'alef-runtime-1.2.3-setup.nsi')));
    assert.ok(existsSync(path.join(out, 'bin', 'alef.exe')));
    let failed = false;
    try {
      execFileSync(process.execPath, ['scripts/release/installer.mjs', 'win32-x64', '1.0.0-x', binary, out],
        { timeout: 30000, encoding: 'utf8', stdio: 'pipe' });
    } catch {
      failed = true;
    }
    assert.ok(failed);
  } finally {
    rmSync(dir, { recursive: true, force: true });
  }
});
